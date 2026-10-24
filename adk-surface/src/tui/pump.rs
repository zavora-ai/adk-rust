//! Reads the engine for the terminal (spec 3.3 §6): the status line, the roster, the queue
//! and the briefing on every change the source signals, and the shown session's feed,
//! resumed after the last `seq` seen. The terminal keeps nothing a reconnect cannot rebuild.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use futures::StreamExt;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;

use super::app::Update;
use crate::engine::Command;
use crate::engine::{FeedFrame, SurfaceEngine};
use crate::error::SurfaceError;
use crate::origin::Caller;

/// How many status reads pass between two reads of the briefing.
const BRIEFING_EVERY: u32 = 12;

/// The tasks reading the engine for one terminal; they stop when it is dropped.
pub(crate) struct Pump {
    session: watch::Sender<String>,
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
        let (sender, receiver) = watch::channel(session);
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

    /// Shows `session` instead.
    pub(crate) fn switch(&self, session: String) {
        self.session.send_replace(session);
    }

    /// Carries out `command` in the background; its outcome comes back as an update.
    pub(crate) fn execute(
        engine: &SurfaceEngine,
        caller: &Caller,
        command: Command,
        tx: &mpsc::UnboundedSender<Update>,
    ) {
        let engine = engine.clone();
        let caller = caller.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let outcome = engine.execute(&caller, command).await.map_err(|error| error.to_string());
            let _ = tx.send(Update::Outcome(Box::new(outcome)));
        });
    }
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
    session: watch::Receiver<String>,
    poll: Duration,
    briefing: Arc<Notify>,
    tx: mpsc::UnboundedSender<Update>,
) {
    let mut round = 0u32;
    let mut briefing_due = true;
    loop {
        let shown = session.borrow().clone();
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

async fn read_feed(
    engine: SurfaceEngine,
    caller: Caller,
    mut session: watch::Receiver<String>,
    poll: Duration,
    tx: mpsc::UnboundedSender<Update>,
) {
    let mut shown = session.borrow_and_update().clone();
    let mut after: Option<u64> = None;
    loop {
        let mut feed = engine.feed(caller.clone(), shown.clone(), after);
        loop {
            tokio::select! {
                changed = session.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    shown = session.borrow_and_update().clone();
                    after = None;
                    break;
                }
                frame = feed.next() => match frame {
                    Some(Ok(frame)) => {
                        match &frame {
                            FeedFrame::Entry { entry } => after = Some(entry.seq),
                            FeedFrame::Reset { .. } => after = None,
                            FeedFrame::Decisions { .. } => {}
                        }
                        if tx.send(Update::Frame { session: shown.clone(), frame }).is_err() {
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
