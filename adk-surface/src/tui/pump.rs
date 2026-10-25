//! Reads the engine for the terminal (spec 3.3 §6): the status line, the roster, the queue
//! and the briefing on every change the source signals, and the shown session's feed,
//! resumed after the last `seq` seen. The terminal keeps nothing a reconnect cannot rebuild.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use futures::StreamExt;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};

use super::app::{Failure, Update};
use super::commands::{CommandContext, TerminalCommand};
use crate::engine::Command;
use crate::engine::{FeedFrame, SurfaceEngine};
use crate::error::SurfaceError;
use crate::model::ThreadItem;
use crate::origin::Caller;
use crate::words;

/// How many status reads pass between two reads of the briefing.
const BRIEFING_EVERY: u32 = 12;

/// How long leaving waits for commands still on their way, such as a Stop all to a kernel
/// over HTTP.
pub(crate) const FINISH_WAIT: Duration = Duration::from_secs(5);

/// The tasks reading the engine for one terminal; they stop when it is dropped.
pub(crate) struct Pump {
    /// The session shown, and how many times it changed: frames carry the count, so the
    /// terminal keeps none read for a session it showed before.
    session: watch::Sender<(String, u64)>,
    briefing: Arc<Notify>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Pump {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Pump {
    /// Starts reading `session` for `caller`, sending what changes to `tx`.
    pub(crate) fn start(
        engine: SurfaceEngine,
        caller: Caller,
        session: String,
        poll: Duration,
        tx: mpsc::UnboundedSender<Update>,
    ) -> Self {
        let (sender, receiver) = watch::channel((session, 0));
        let briefing = Arc::new(Notify::new());
        let state = tokio::spawn(read_state(
            engine.clone(),
            caller.clone(),
            receiver.clone(),
            poll,
            Arc::clone(&briefing),
            tx.clone(),
        ));
        let feed = tokio::spawn(read_feed(engine, caller, receiver, poll, tx));
        Self { session: sender, briefing, tasks: vec![state, feed] }
    }

    /// Reads "While you were away" again at once.
    pub(crate) fn refresh_briefing(&self) {
        self.briefing.notify_one();
    }

    /// Shows `session` instead: the terminal's [`App`](super::app::App) switched to it.
    pub(crate) fn switch(&self, session: String) {
        self.session.send_modify(|(shown, generation)| {
            *shown = session;
            *generation += 1;
        });
    }

    /// Carries out `command` in the background; its outcome comes back as an update.
    #[cfg(test)]
    pub(crate) fn execute(
        engine: &SurfaceEngine,
        caller: &Caller,
        command: Command,
        tx: &mpsc::UnboundedSender<Update>,
    ) {
        tokio::spawn(run_command(engine.clone(), caller.clone(), command, tx.clone()));
    }
}

async fn run_command(
    engine: SurfaceEngine,
    caller: Caller,
    command: Command,
    tx: mpsc::UnboundedSender<Update>,
) {
    let decision = match &command {
        Command::Decide { decision_id, .. } => Some(decision_id.clone()),
        _ => None,
    };
    let outcome = engine
        .execute(&caller, command)
        .await
        .map_err(|error| failure(&error, decision.as_deref()));
    let _ = tx.send(Update::Outcome(Box::new(outcome)));
}

/// The commands a terminal sent, so leaving waits for those still on their way instead of
/// dropping them: a Stop all sent just before leaving still reaches the agents (S-I10).
#[derive(Default)]
pub(crate) struct Commands {
    running: JoinSet<()>,
    what: Arc<Mutex<HashMap<tokio::task::Id, String>>>,
}

impl Commands {
    /// Carries out `command` in the background; its outcome comes back as an update.
    pub(crate) fn execute(
        &mut self,
        engine: &SurfaceEngine,
        caller: &Caller,
        command: Command,
        tx: &mpsc::UnboundedSender<Update>,
    ) {
        let what = match &command {
            Command::StopAll { .. } => words::STOP_ALL,
            Command::ResumeAll => words::RESUME_ALL,
            Command::Decide { .. } => "your answer",
            Command::Send { .. } => "your message",
            Command::SetMode { .. } => "the mode change",
            _ => "a command",
        };
        let handle =
            self.running.spawn(run_command(engine.clone(), caller.clone(), command, tx.clone()));
        self.track(handle.id(), what.to_string());
    }

    /// Runs `command`, a slash command the embedder added, with `args` in `context`'s session;
    /// what it returns comes back as the `serial`-th command's output (spec 3.3 §5.2).
    pub(crate) fn run_added(
        &mut self,
        command: Arc<dyn TerminalCommand>,
        args: String,
        context: CommandContext,
        serial: u64,
        tx: &mpsc::UnboundedSender<Update>,
    ) {
        let what = if args.is_empty() {
            format!("/{}", command.name())
        } else {
            format!("/{} {args}", command.name())
        };
        let tx = tx.clone();
        let handle = self.running.spawn(async move {
            let result = command.run(&args, &context).await.map_err(|error| error.message);
            let _ = tx.send(Update::CommandDone { serial, result });
        });
        self.track(handle.id(), what);
    }

    /// Notes what the task `id` carries out, and reaps those finished.
    fn track(&mut self, id: tokio::task::Id, what: String) {
        self.what.lock().unwrap_or_else(|e| e.into_inner()).insert(id, what);
        // Finished commands are reaped as new ones go, so the set holds only those running.
        while let Some(done) = self.running.try_join_next_with_id() {
            let id = match done {
                Ok((id, ())) => id,
                Err(error) => error.id(),
            };
            self.what.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        }
    }

    /// Whether a command is still on its way.
    pub(crate) fn running(&self) -> bool {
        !self.running.is_empty()
    }

    /// Waits up to `wait` for the commands still on their way; returns what did not finish
    /// in that time, which is then abandoned.
    pub(crate) async fn finish(mut self, wait: Duration) -> Vec<String> {
        let what = Arc::clone(&self.what);
        let drained = tokio::time::timeout(wait, async {
            while let Some(done) = self.running.join_next_with_id().await {
                let id = match done {
                    Ok((id, ())) => id,
                    Err(error) => error.id(),
                };
                what.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            }
        })
        .await;
        if drained.is_ok() {
            return Vec::new();
        }
        let left = what.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        self.running.abort_all();
        left
    }
}

/// What a failed command shows (spec 3.3 §8): the engine's words, never an inbox id. A
/// decision no longer found was answered somewhere else, so it reads as decided (§8.1).
pub(crate) fn failure(error: &SurfaceError, decision: Option<&str>) -> Failure {
    if let (Some(id), SurfaceError::NotFound { .. }) = (decision, error) {
        return Failure {
            text: format!("{} — answered somewhere else.", words::DECIDED),
            gone: Some(id.to_string()),
        };
    }
    let mut text = match error {
        SurfaceError::Source(error) => error.message.clone(),
        other => other.to_string(),
    };
    if let Some(id) = decision.filter(|id| !id.is_empty()) {
        text = text.replace(&format!("'{id}'"), "this decision").replace(id, "this decision");
    }
    Failure { text, gone: None }
}

/// The start of today, local time: "While you were away" in the terminal counts from there.
pub(crate) fn start_of_today() -> DateTime<Utc> {
    let now = Local::now();
    now.date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|midnight| midnight.and_local_timezone(Local).earliest())
        .map_or_else(Utc::now, |midnight| midnight.with_timezone(&Utc))
}

async fn read_state(
    engine: SurfaceEngine,
    caller: Caller,
    session: watch::Receiver<(String, u64)>,
    poll: Duration,
    briefing: Arc<Notify>,
    tx: mpsc::UnboundedSender<Update>,
) {
    let mut round = 0u32;
    let mut briefing_due = true;
    loop {
        let shown = session.borrow().0.clone();
        let mut sent = true;
        match engine.status(&caller, Some(&shown)).await {
            Ok(status) => sent &= tx.send(Update::Status(status)).is_ok(),
            // A session not yet created reads as the default mode.
            Err(SurfaceError::NotFound { .. }) | Err(SurfaceError::Invalid(_)) => {
                if let Ok(status) = engine.status(&caller, None).await {
                    sent &= tx.send(Update::Status(status)).is_ok();
                }
            }
            Err(error) => sent &= tx.send(Update::Error(error.to_string())).is_ok(),
        }
        if let Ok(agents) = engine.agents(&caller).await {
            sent &= tx.send(Update::Agents(agents)).is_ok();
        }
        let lines = engine.queued(&caller, &shown);
        sent &= tx.send(Update::Queued { session: shown, lines }).is_ok();
        if (briefing_due || round.is_multiple_of(BRIEFING_EVERY))
            && let Ok(briefing) = engine.briefing(&caller, start_of_today()).await
        {
            sent &= tx.send(Update::Briefing(briefing)).is_ok();
        }
        briefing_due = false;
        if !sent {
            return;
        }
        round = round.wrapping_add(1);
        tokio::select! {
            () = engine.source().changed(poll) => {}
            () = tokio::time::sleep(poll) => {}
            () = briefing.notified() => briefing_due = true,
        }
    }
}

/// Reads the decisions of the shown session that are no longer open and sends how each was
/// decided, once: the thread shows it at the decision's place (spec 3.3 §8.1 "Decided").
async fn send_decided(
    engine: &SurfaceEngine,
    caller: &Caller,
    ids: Vec<String>,
    generation: u64,
    known: &mut HashSet<String>,
    tx: &mpsc::UnboundedSender<Update>,
) -> bool {
    for id in ids {
        if known.contains(&id) {
            continue;
        }
        if let Ok(card) = engine.decision(caller, &id).await
            && card.decided.is_some()
        {
            known.insert(id);
            if tx.send(Update::Decided { generation, card: Box::new(card) }).is_err() {
                return false;
            }
        }
    }
    true
}

async fn read_feed(
    engine: SurfaceEngine,
    caller: Caller,
    mut session: watch::Receiver<(String, u64)>,
    poll: Duration,
    tx: mpsc::UnboundedSender<Update>,
) {
    let (mut shown, mut generation) = session.borrow_and_update().clone();
    let mut after: Option<u64> = None;
    let mut open: HashSet<String> = HashSet::new();
    let mut known: HashSet<String> = HashSet::new();
    loop {
        let mut feed = engine.feed(caller.clone(), shown.clone(), after);
        loop {
            tokio::select! {
                // A switch is taken before another frame of the session shown before.
                biased;
                changed = session.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    (shown, generation) = session.borrow_and_update().clone();
                    after = None;
                    open.clear();
                    known.clear();
                    break;
                }
                frame = feed.next() => match frame {
                    Some(Ok(frame)) => {
                        let mut decided = Vec::new();
                        match &frame {
                            FeedFrame::Entry { entry } => {
                                after = Some(entry.seq);
                                for item in &entry.items {
                                    if let ThreadItem::Decision { decision_id: Some(id), .. } = item
                                        && !open.contains(id)
                                    {
                                        decided.push(id.clone());
                                    }
                                }
                            }
                            FeedFrame::Reset { .. } => after = None,
                            FeedFrame::Decisions { cards } => {
                                let now: HashSet<String> =
                                    cards.iter().map(|card| card.id.clone()).collect();
                                decided.extend(open.difference(&now).cloned());
                                open = now;
                            }
                        }
                        let update = Update::Frame { session: shown.clone(), generation, frame };
                        if tx.send(update).is_err()
                            || !send_decided(&engine, &caller, decided, generation, &mut known, &tx)
                                .await
                        {
                            return;
                        }
                    }
                    // A new session exists once its first turn starts: read it again.
                    Some(Err(SurfaceError::NotFound { .. })) | None => {
                        tokio::time::sleep(poll).await;
                        break;
                    }
                    Some(Err(error)) => {
                        if tx.send(Update::Error(error.to_string())).is_err() {
                            return;
                        }
                        tokio::time::sleep(poll * 4).await;
                        break;
                    }
                },
            }
        }
    }
}
