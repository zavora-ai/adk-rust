//! Projection: inbox entries, session events, process states and ledger rows become cards,
//! thread items, agent rows, status and briefing lines, once, in Rust (spec 3.3 §6, §7.1, §8).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use adk_core::{
    ApprovalAction, ApprovalRequest, ApprovalState, Event, Part, RunState, ToolConfirmationRequest,
};
use adk_harness::HarnessMode;
use adk_harness::take_over::TAKE_OVER_TOOL;
use adk_kernel::control::api::{AwayKind, AwayLine};
use adk_kernel::proc::{KERNEL_AGENT, LedgerEntry};
use adk_kernel::{AGENDA_TOOL_NAME, AgendaItem, AgendaOrigin, QuietHours, Wake};
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use serde_json::Value;

use crate::answers::AnswerRecord;
use crate::hints::CardHint;
use crate::model::{
    AgentRef, AgentRow, AgentStatus, Avatar, BriefingLine, CallView, CardStatus, Decided,
    DecidedAnswer, DecisionCard, FeedEntry, Mode, ModeView, Pill, QuietView, Reversibility,
    Speaker, SpendView, ThreadItem, TimelineStep,
};
use crate::origin::{Caller, SurfaceKind};
use crate::rules::{self, Answer, DecisionFacts};
use crate::source::{AgentState, SpendToday};
use crate::words::{self, SaidWhat};

/// How long a call's text may be before it is shown truncated (spec 3.3 §7.1).
pub const CALL_TEXT_LIMIT: usize = 2_000;

/// Somewhere a decision is also shown, for "Also on your phone and in Telegram"
/// (spec 3.3 §7.1).
///
/// # Example
///
/// ```rust
/// use adk_surface::Place;
///
/// assert_eq!(Place::chat("Telegram").phrase, "in Telegram");
/// assert_eq!(Place::phone().phrase, "on your phone");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// The surface.
    pub surface: SurfaceKind,
    /// The place in words: "on your phone", "in Telegram".
    pub phrase: String,
}

impl Place {
    /// The web home.
    pub fn web() -> Self {
        Self { surface: SurfaceKind::Web, phrase: words::IN_THE_HOME_APP.to_string() }
    }

    /// The phone.
    pub fn phone() -> Self {
        Self { surface: SurfaceKind::Phone, phrase: words::ON_YOUR_PHONE.to_string() }
    }

    /// The terminal.
    pub fn terminal() -> Self {
        Self { surface: SurfaceKind::Terminal, phrase: words::IN_THE_TERMINAL.to_string() }
    }

    /// The desktop tray.
    pub fn tray() -> Self {
        Self { surface: SurfaceKind::Tray, phrase: words::FROM_THE_TRAY.to_string() }
    }

    /// A chat channel.
    pub fn chat(channel: &str) -> Self {
        Self { surface: SurfaceKind::Chat, phrase: words::in_channel(channel) }
    }
}

type Classify<T> = Arc<dyn Fn(&ApprovalRequest) -> T + Send + Sync>;

/// What the engine needs to project: the person's time zone, quiet time, where decisions are
/// shown, and how calls are read.
#[derive(Clone)]
pub(crate) struct Projector {
    pub(crate) offset: FixedOffset,
    pub(crate) quiet: Option<QuietHours>,
    pub(crate) places: Vec<Place>,
    pub(crate) names: BTreeMap<String, String>,
    pub(crate) heading: Classify<String>,
    pub(crate) money: Classify<bool>,
    /// Whether the source rates a call high risk whatever the request says (spec 3.2 K-I9).
    pub(crate) rates_high: Classify<bool>,
    pub(crate) reversibility: Classify<Reversibility>,
    /// The engine's cancel window for a spoken "allow once", in whole seconds, which a voice
    /// card advertises.
    pub(crate) voice_window_secs: u32,
}

impl fmt::Debug for Projector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Projector")
            .field("offset", &self.offset)
            .field("quiet", &self.quiet)
            .field("places", &self.places)
            .finish_non_exhaustive()
    }
}

impl Projector {
    pub(crate) fn new(offset: FixedOffset) -> Self {
        Self {
            offset,
            quiet: None,
            places: Vec::new(),
            names: BTreeMap::new(),
            heading: Arc::new(heading),
            money: Arc::new(spends_money),
            rates_high: Arc::new(|_| false),
            reversibility: Arc::new(reversibility),
            voice_window_secs: rules::VOICE_CANCEL_WINDOW_SECS,
        }
    }

    /// `at` as local `HH:MM`.
    pub(crate) fn hh_mm(&self, at: DateTime<Utc>) -> String {
        at.with_timezone(&self.offset).format("%H:%M").to_string()
    }

    pub(crate) fn agent(&self, name: &str) -> AgentRef {
        let display_name = self.names.get(name).cloned().unwrap_or_else(|| humanize(name));
        AgentRef { name: name.to_string(), avatar: avatar(&display_name), display_name }
    }

    /// The agent a suggestion would add, as its card shows it (spec 3.3 §5.7): `None` for
    /// every other request.
    pub(crate) fn adds_agent(
        &self,
        request: &ApprovalRequest,
    ) -> Option<crate::agents::AgentCardView> {
        if request.tool_name != adk_kernel::agents::AGENT_CREATE_TOOL_NAME {
            return None;
        }
        let suggester = self.agent(&agent_name(request)).display_name;
        let area = request.args_redacted.get("area").and_then(Value::as_str).unwrap_or_default();
        crate::agents::AgentCardView::of_request(
            request,
            &suggester,
            &crate::agents::area_name(area),
        )
    }

    /// The facts the §7.2 rules read. A feature's `hint` may only make them stricter: mark
    /// money or take **Always allow** away, never add an answer or change the risk
    /// (spec 3.3 §7.1, §7.2).
    pub(crate) fn facts(
        &self,
        request: &ApprovalRequest,
        always_label: Option<String>,
        hint: &CardHint,
    ) -> DecisionFacts {
        // A suggested agent is added or not, never always or edited (spec 3.3 §7.2); long
        // instructions are expanded before it is added, as a long call is (S-I13).
        if let Some(agent) = self.adds_agent(request) {
            let assessed = adk_kernel::agents::AgentRecord::new(
                String::new(),
                agent.draft.clone().into_definition(),
                adk_kernel::agents::AgentRecordState::Proposed,
                adk_kernel::agents::AgentOrigin::ByHand,
                adk_core::principal::SubjectRef::human(&request.scope.user_id),
                request.created_at,
            );
            return DecisionFacts {
                risk: request.risk.into(),
                money: adk_harness::assess_agent(&assessed).money || hint.marks_money(),
                truncated: agent.instructions_truncated,
                allows: request
                    .allowed_actions
                    .iter()
                    .copied()
                    .filter(|action| {
                        matches!(action, ApprovalAction::Approve | ApprovalAction::Reject)
                    })
                    .collect(),
                always_label: None,
                adds_agent: true,
            };
        }
        // Edit replaces the call's arguments with what the surface sends back, so it is
        // offered only where the card shows those arguments (spec 3.3 §7.2): never over a
        // redacted view or an elicitation's form, which would be submitted as written. A take
        // over is answered with the secret the person types, or refused (§8.2, §10 H2).
        let own_args = shows_own_arguments(request);
        let take_over = request.tool_name == TAKE_OVER_TOOL;
        let risk = if (self.rates_high)(request) {
            request.risk.max(adk_core::ApprovalRisk::High)
        } else {
            request.risk
        };
        DecisionFacts {
            risk: risk.into(),
            money: (self.money)(request) || hint.marks_money(),
            truncated: call_view(request).truncated,
            allows: request
                .allowed_actions
                .iter()
                .copied()
                .filter(|action| match action {
                    ApprovalAction::Reject => true,
                    ApprovalAction::Edit => own_args && !take_over,
                    ApprovalAction::Approve | ApprovalAction::Respond => !take_over,
                })
                .collect(),
            always_label: always_label.filter(|_| !take_over && !hint.removes_always()),
            adds_agent: false,
        }
    }

    pub(crate) fn decided(
        &self,
        request: &ApprovalRequest,
        caller: &Caller,
        answer: Option<&AnswerRecord>,
    ) -> Option<Decided> {
        if request.state == ApprovalState::Pending {
            return None;
        }
        let last = request.decisions.last();
        let answer_kind = match request.state {
            ApprovalState::Approved => match last.map(|decision| decision.kind) {
                Some(ApprovalAction::Edit) => DecidedAnswer::Edited,
                _ => DecidedAnswer::Yes,
            },
            ApprovalState::Rejected => DecidedAnswer::No,
            ApprovalState::Responded => DecidedAnswer::Replied,
            ApprovalState::Pending | ApprovalState::Expired | ApprovalState::Cancelled => {
                DecidedAnswer::Ended
            }
        };
        let at = last
            .map(|decision| decision.decided_at)
            .or(request.resolved_at)
            .unwrap_or(request.created_at);
        let at_text = self.hh_mm(at);
        let approver = last.filter(|_| answer_kind != DecidedAnswer::Ended);
        let by_you = approver.is_some_and(|decision| decision.approver.id == caller.user_id());
        let by = match approver {
            Some(_) if by_you => "You".to_string(),
            Some(decision) => decision.approver.id.clone(),
            None => String::new(),
        };
        let via = answer
            .filter(|record| {
                record.approval_id == request.id
                    && approver.is_some_and(|decision| decision.approver == record.by)
            })
            .map(|record| record.via.clone());
        let said = match answer_kind {
            DecidedAnswer::Yes => Some(SaidWhat::Yes),
            DecidedAnswer::No => Some(SaidWhat::No),
            DecidedAnswer::Edited => Some(SaidWhat::Edited),
            DecidedAnswer::Replied => Some(SaidWhat::Replied),
            DecidedAnswer::Ended | DecidedAnswer::Unknown => None,
        };
        let text = match said {
            Some(what) => {
                let phrase = via.as_ref().map(crate::origin::Via::phrase);
                words::said((!by_you).then_some(by.as_str()), what, phrase.as_deref(), &at_text)
            }
            None => words::ENDED_WITHOUT_AN_ANSWER.to_string(),
        };
        Some(Decided { by, by_you, via, at, at_text, answer: answer_kind, text, scope_text: None })
    }

    /// The card of `request` as `caller` sees it (spec 3.3 §7.1), with a feature's `hint`
    /// ahead of the engine's own headings and reversibility.
    pub(crate) fn card(
        &self,
        request: &ApprovalRequest,
        caller: &Caller,
        always_label: Option<String>,
        answer: Option<&AnswerRecord>,
        now: DateTime<Utc>,
        hint: &CardHint,
    ) -> DecisionCard {
        let facts = self.facts(request, always_label, hint);
        let chat = rules::row(caller) == rules::Row::Chat;
        // A chat card leaves the machine through someone else's servers: it carries the
        // agent, the risk and a short summary, never the call's arguments (S-I5).
        let (heading, summary, call) = if chat {
            let full = call_view(request);
            let call = CallView {
                tool: full.tool.clone(),
                args: Value::Null,
                text: full.tool,
                truncated: full.truncated,
            };
            let heading = hint.chat_heading.clone().unwrap_or_else(|| chat_heading(request));
            (heading, None, call)
        } else {
            let heading = hint.heading.clone().unwrap_or_else(|| (self.heading)(request));
            (heading, self.kernel_summary(request, now), call_view(request))
        };
        let decided = self.decided(request, caller, answer);
        let mut allowed = rules::allowed_answers(caller, &facts);
        // The window the engine waits, not the default, so a voice renderer counts down the
        // time the answer can really still be cancelled.
        if allowed.cancel_window_secs.is_some() {
            allowed.cancel_window_secs = Some(self.voice_window_secs);
        }
        // A suggestion is done or left for now, a question confirmed or forgotten (§8.1).
        if let Some((yes, no)) = kernel_answer_words(request) {
            for option in &mut allowed.options {
                match option.answer {
                    Answer::AllowOnce if !option.needs_presence => option.label = yes.to_string(),
                    Answer::Deny => option.label = no.to_string(),
                    Answer::AllowOnce | Answer::AlwaysAllow | Answer::Edit => {}
                }
            }
        }
        if decided.is_some() {
            allowed.options.clear();
            allowed.open_in_home = false;
            allowed.cancel_window_secs = None;
            allowed.note = None;
        }
        let since = self.hh_mm(request.created_at);
        let mut timeline: Vec<String> = hint.first_step.iter().cloned().collect();
        timeline.push(words::asked_at(&since));
        let until = decided.as_ref().map_or(now, |decided| decided.at);
        if self.waited_through_quiet(request.created_at, until) {
            timeline.push(words::WAITED_THROUGH_QUIET_TIME.to_string());
        }
        timeline.push(match &decided {
            Some(decided) => decided.text.clone(),
            None => words::WAITING_FOR_YOU.to_string(),
        });
        let last = timeline.len() - 1;
        let timeline = timeline
            .into_iter()
            .enumerate()
            .map(|(index, text)| TimelineStep { text, current: index == last })
            .collect();
        let adds_agent = self.adds_agent(request);
        let reversibility = match adds_agent {
            Some(_) => Reversibility::CanBeRemoved,
            None => hint.reversibility.unwrap_or_else(|| (self.reversibility)(request)),
        };
        let also: Vec<String> = self
            .places
            .iter()
            .filter(|place| place.surface != caller.surface)
            .map(|place| place.phrase.clone())
            .collect();
        let (status, status_text) = match decided {
            Some(_) => (CardStatus::Decided, words::DECIDED),
            None => (CardStatus::Waiting, words::WAITING_FOR_YOU),
        };
        DecisionCard {
            id: request.id.clone(),
            version: request.version,
            session_id: request.scope.session_id.clone(),
            agent: self.agent(&agent_name(request)),
            waiting_since: request.created_at,
            since_text: words::since(&since),
            status,
            status_text: status_text.to_string(),
            heading,
            summary,
            call,
            timeline,
            allowed,
            risk: facts.risk,
            risk_text: facts.risk.words().to_string(),
            reversibility,
            reversibility_text: match reversibility {
                Reversibility::CantBeUnsent => words::CANT_BE_UNSENT,
                Reversibility::CantBeUndone => words::CANT_BE_UNDONE,
                Reversibility::CanBeRemoved => crate::agents::words::YOU_CAN_REMOVE_IT_LATER,
                Reversibility::CanBeChangedBack => words::YOU_CAN_CHANGE_IT_BACK,
            }
            .to_string(),
            money: facts.money,
            also_shown: words::also_shown(&also),
            decided,
            adds_agent,
            // Like the summary, never on a chat card (S-I5).
            detail: hint.detail.clone().filter(|_| !chat),
        }
    }

    /// What a decision the kernel itself asks means for the person, beyond its heading: who
    /// suggests an agenda item and when it runs once allowed, what a driver mount starts
    /// (spec 3.3 §7.1, §8.1). `None` for every other call, whose heading says it all.
    pub(crate) fn kernel_summary(
        &self,
        request: &ApprovalRequest,
        now: DateTime<Utc>,
    ) -> Option<String> {
        let operation = request.tool_name.strip_prefix(KERNEL_TOOL_PREFIX)?;
        if request.tool_name == AGENDA_TOOL_NAME {
            let Some(item) = agenda_item(request) else {
                return Some(words::AGENTS_SUGGEST.to_string());
            };
            if item.origin.is_question() {
                return Some(words::NOTICED_SOMETHING.to_string());
            }
            let who = match &item.origin {
                AgendaOrigin::User => words::YOU_SUGGESTED_THIS.to_string(),
                AgendaOrigin::Agent { .. } => {
                    words::suggested_by(Some(&self.agent(&agent_name(request)).display_name))
                }
                // Consolidation, and any origin a later kernel adds: the kernel's own work.
                _ => words::suggested_by(None),
            };
            let when = match &item.when {
                Some(wake) => self.wake_words(wake, now),
                None => words::RIGHT_AWAY.to_string(),
            };
            let mut text = format!("{who} {}", words::once_you_say_yes(&when));
            if let Some(steps) = item.plan.as_ref().map(|plan| plan.steps.len()).filter(|n| *n > 0)
            {
                text.push(' ');
                text.push_str(&words::plan_of(steps));
            }
            if let Some(cap) = item.budget.as_ref().and_then(|budget| budget.max_cost_micro_usd) {
                text.push(' ');
                text.push_str(&words::may_spend(cap));
            }
            return Some(text);
        }
        if operation != DRIVER_MOUNT_OPERATION {
            return None;
        }
        request.args_redacted.get("commandLine").and_then(Value::as_str).map(words::driver_starts)
    }

    /// When a wake fires, in words, after "it runs": "every 5 minutes", "at 07:30".
    fn wake_words(&self, wake: &Wake, now: DateTime<Utc>) -> String {
        match wake {
            Wake::At(at) if *at <= now => words::RIGHT_AWAY.to_string(),
            Wake::At(at) => {
                let local = at.with_timezone(&self.offset);
                let today = now.with_timezone(&self.offset).date_naive();
                let time = local.format("%H:%M");
                if local.date_naive() == today {
                    format!("at {time}")
                } else if today.succ_opt() == Some(local.date_naive()) {
                    format!("tomorrow at {time}")
                } else {
                    format!("on {} at {time}", local.format("%a %-d %b"))
                }
            }
            Wake::Cron { expr, .. } => self.cron_words(expr, now),
            Wake::FileChange { root, glob, .. } => {
                format!("when a file matching {glob} changes in {}", root.display())
            }
            Wake::Webhook { hook } => format!("when {hook} calls in"),
            Wake::Queue { source } => format!("when a message arrives from {source}"),
            Wake::Approval { .. } => "once a decision it waits on is answered".to_string(),
            Wake::RunFinished { .. } => "after another of your agents' tasks finishes".to_string(),
            Wake::BudgetReplenished { .. } => "once the day's budget allows it".to_string(),
            Wake::Manual => "when you start it".to_string(),
            // A wake a later kernel adds.
            _ => "later".to_string(),
        }
    }

    /// A cron schedule (UTC, five fields, or six or seven from the second) in words, in the
    /// person's time zone: "every 5 minutes", "every weekday at 07:00"; any other schedule
    /// as it is written.
    fn cron_words(&self, expr: &str, now: DateTime<Utc>) -> String {
        let written = || format!("on the schedule {expr}");
        let all: Vec<&str> = expr.split_whitespace().collect();
        let fields = match all.len() {
            5 => &all[..],
            6 | 7 if all[0] == "0" && all.get(6).is_none_or(|year| *year == "*") => &all[1..6],
            _ => return written(),
        };
        let &[minute, hour, day, month, weekday] = fields else {
            return written();
        };
        let every = |field: &str| match field {
            "*" => Some(1),
            _ => field.strip_prefix("*/").and_then(|step| step.parse::<u32>().ok()),
        };
        let number = |field: &str| field.parse::<u32>().ok();
        if day != "*" || month != "*" {
            return written();
        }
        match (every(minute), number(minute), every(hour), number(hour)) {
            (Some(1), _, Some(1), _) if weekday == "*" => "every minute".to_string(),
            (Some(step), _, Some(1), _) if weekday == "*" => format!("every {step} minutes"),
            (_, Some(_), Some(1), _) if weekday == "*" => "every hour".to_string(),
            (_, Some(_), Some(step), _) if weekday == "*" => format!("every {step} hours"),
            (_, Some(minute), _, Some(hour)) => {
                let Some(at) = now.date_naive().and_hms_opt(hour, minute, 0) else {
                    return written();
                };
                let at = Utc.from_utc_datetime(&at);
                let local = at.with_timezone(&self.offset);
                let time = local.format("%H:%M");
                let days = match weekday.to_ascii_uppercase().as_str() {
                    "*" => "every day".to_string(),
                    // Another day where the person lives: the days would read wrong.
                    _ if local.date_naive() != at.date_naive() => return written(),
                    "1-5" | "MON-FRI" => "every weekday".to_string(),
                    other => match WEEKDAYS.iter().find(|(number, name, _)| {
                        *number == other || name.eq_ignore_ascii_case(other)
                    }) {
                        Some((_, _, day)) => format!("every {day}"),
                        None => return written(),
                    },
                };
                format!("{days} at {time}")
            }
            _ => written(),
        }
    }

    /// Whether `[from, until]` overlaps quiet time.
    pub(crate) fn waited_through_quiet(&self, from: DateTime<Utc>, until: DateTime<Utc>) -> bool {
        let Some(quiet) = self.quiet else {
            return false;
        };
        if quiet.contains(from) || quiet.contains(until) {
            return true;
        }
        let offset = self.quiet_offset(&quiet);
        let first = from.with_timezone(&offset).date_naive();
        let last = until.with_timezone(&offset).date_naive();
        let mut day = first;
        while day <= last {
            if let Some(start) = offset.from_local_datetime(&day.and_time(quiet.start)).earliest()
                && start > from
                && start <= until
            {
                return true;
            }
            day = match day.succ_opt() {
                Some(next) => next,
                None => break,
            };
        }
        false
    }

    fn quiet_offset(&self, quiet: &QuietHours) -> FixedOffset {
        quiet
            .utc_offset_minutes
            .and_then(|minutes| FixedOffset::east_opt(minutes * 60))
            .unwrap_or(self.offset)
    }

    pub(crate) fn quiet_view(&self, now: DateTime<Utc>) -> Option<QuietView> {
        let quiet = self.quiet?;
        if quiet.start == quiet.end {
            return None;
        }
        let start = quiet.start.format("%H:%M").to_string();
        let end = quiet.end.format("%H:%M").to_string();
        let active = quiet.contains(now);
        let text = if active { words::quiet_until(&end) } else { words::quiet_from(&start) };
        Some(QuietView { active, start, end, text })
    }

    /// The event at `seq`, its held call linked to the decision [`links`] gives that `seq`;
    /// `answered` holds the calls the session's later events answer ([`answered_calls`]).
    pub(crate) fn entry(
        &self,
        seq: u64,
        event: &Event,
        links: &HashMap<u64, Link>,
        answered: &HashSet<String>,
    ) -> FeedEntry {
        FeedEntry {
            seq,
            at: event.timestamp,
            at_text: self.hh_mm(event.timestamp),
            items: self.items(event, links.get(&seq), answered),
        }
    }

    fn items(
        &self,
        event: &Event,
        link: Option<&Link>,
        answered: &HashSet<String>,
    ) -> Vec<ThreadItem> {
        let mut items = Vec::new();
        let from_person = event.author == "user";
        // A call a person asked for through a drawn form (spec 3.3 §7.4 rule 5).
        let requested = crate::genui::requested_via(event);
        if let Some(content) = &event.llm_response.content {
            let mut text = String::new();
            for part in &content.parts {
                match part {
                    Part::Text { text: chunk } if !event.llm_response.partial => {
                        text.push_str(chunk);
                    }
                    // The person's own call: its DrawnSent item shows it, not the agent's work.
                    Part::FunctionCall { id, .. }
                        if requested.is_some() && crate::genui::is_requested(id.as_ref()) => {}
                    Part::FunctionCall { name, args, id, .. } => {
                        items.push(ThreadItem::Activity {
                            tool: name.clone(),
                            text: describe_call(name, args),
                            call_id: id.clone(),
                        });
                    }
                    // A held call's placeholder answer: its Decision item shows it.
                    Part::FunctionResponse { function_response, .. }
                        if adk_core::is_confirmation_pending(&function_response.response) => {}
                    // A call a person asked for that the agent's rules refused before holding it.
                    Part::FunctionResponse { function_response, id: Some(id), .. }
                        if requested.is_some() && crate::genui::is_requested(Some(id)) =>
                    {
                        if let Some(via) = &requested {
                            items.push(crate::genui::sent(
                                via,
                                &function_response.name,
                                id,
                                event,
                                &self.hh_mm(event.timestamp),
                                Some(&self.agent(&event.author).display_name),
                                None,
                            ));
                        }
                    }
                    Part::FunctionResponse { function_response, id, .. } => {
                        // A drawing, or a refused one, when the event proves a render tool
                        // made it (spec 3.3 §7.4 rule 5).
                        #[cfg(feature = "genui")]
                        {
                            if let Some(item) = crate::genui::project(
                                self,
                                event,
                                &function_response.name,
                                &function_response.response,
                                id.as_ref(),
                            ) {
                                items.push(item);
                                continue;
                            }
                        }
                        let ok = !response_failed(&function_response.response);
                        let text = if !ok
                            && crate::genui::is_requested(id.as_ref())
                            && crate::genui::refused_on_resume(
                                &function_response.name,
                                &function_response.response,
                            ) {
                            words::cant_use_any_more(
                                &self.agent(&event.author).display_name,
                                &function_response.name,
                            )
                        } else if ok {
                            words::DONE.to_string()
                        } else {
                            words::FAILED.to_string()
                        };
                        items.push(ThreadItem::Result {
                            tool: function_response.name.clone(),
                            call_id: id.clone(),
                            ok,
                            text,
                        });
                    }
                    _ => {}
                }
            }
            let text = text.trim();
            // The event that holds a call carries the runtime's own notice, not the agent's
            // words: its Decision item says it plainly (§8). A requested call's line is for
            // the model; its DrawnSent item says it to the person.
            if !text.is_empty() && event.actions.tool_confirmation.is_none() && requested.is_none()
            {
                let message = if from_person {
                    ThreadItem::Message {
                        from: Speaker::You,
                        author: "You".to_string(),
                        text: text.to_string(),
                    }
                } else {
                    ThreadItem::Message {
                        from: Speaker::Agent,
                        author: self.agent(&event.author).display_name,
                        text: text.to_string(),
                    }
                };
                items.insert(0, message);
            }
        }
        if let Some(held) = &event.actions.tool_confirmation {
            // "Waiting for you" only while the caller's own decision is open (§8.1); a call
            // routed to other approvers, or to no one, waits for an answer; one answered since
            // is decided.
            let ran = held.function_call_id.as_ref().is_some_and(|id| answered.contains(id));
            let text = match link {
                Some(link) if link.pending && !ran => words::WAITING_FOR_YOU,
                Some(_) => words::DECIDED,
                None if ran => words::DECIDED,
                None => words::WAITING_FOR_AN_ANSWER,
            };
            if let (Some(via), Some(call_id)) = (&requested, &held.function_call_id) {
                items.push(crate::genui::sent(
                    via,
                    &held.tool_name,
                    call_id,
                    event,
                    &self.hh_mm(event.timestamp),
                    None,
                    link.map(|link| link.id.clone()),
                ));
            }
            items.push(ThreadItem::Decision {
                decision_id: link.map(|link| link.id.clone()),
                tool: held.tool_name.clone(),
                text: text.to_string(),
            });
        }
        items
    }

    pub(crate) fn agent_row(&self, state: &AgentState) -> AgentRow {
        let (status, status_text) = match state.state {
            RunState::Running | RunState::Queued => {
                (AgentStatus::Working, words::WORKING.to_string())
            }
            RunState::Waiting if state.waiting_on_decision && !state.suspended => {
                (AgentStatus::WaitingForYou, words::WAITING_FOR_YOU.to_string())
            }
            RunState::Waiting => match state.wake_at.filter(|_| !state.suspended) {
                Some(at) => (AgentStatus::Asleep, words::asleep_until(&self.hh_mm(at))),
                None => (AgentStatus::Asleep, words::ASLEEP.to_string()),
            },
            RunState::Completed | RunState::Cancelled => {
                (AgentStatus::Done, words::DONE.to_string())
            }
            RunState::Failed => (AgentStatus::Failed, words::FAILED.to_string()),
            // A state a later runtime adds shows as idle, never as working or done.
            _ => (AgentStatus::Asleep, words::ASLEEP.to_string()),
        };
        AgentRow {
            agent: self.agent(&state.name),
            status,
            status_text,
            wake_at: state.wake_at.filter(|_| status == AgentStatus::Asleep && !state.suspended),
            session_id: state.session_id.clone(),
            area: None,
            mode: None,
            added: false,
            can_change: false,
        }
    }

    /// The line of "While you were away" a ledger row makes, or `None` for a row that is no
    /// event of its own (spec 3.3 §5.1, §10 K4): the line the kernel's `GET /kernel/away`
    /// lists for it ([`AwayLine`]), so every surface shows the same lines and the kernel's
    /// own housekeeping (wakes, its processes' state changes) is never one.
    pub(crate) fn briefing_line(&self, entry: &LedgerEntry) -> Option<BriefingLine> {
        let line = AwayLine::of(entry)?;
        // A decision says which answer it was, whether its row is the harness's or the kernel's.
        let denied = line.effect.as_deref() == Some("deny");
        let at = entry.utc();
        let agent = self.ledger_agent(&entry.agent);
        let who = agent.clone().unwrap_or_else(|| words::YOUR_AGENTS.to_string());
        // The `/proc` ledger writes the state a process entered as the row's effect (spec 3.2
        // §7.10); the loop guard's row carries none.
        let failed = entry.effect.as_deref() == Some("failed");
        // Mid-sentence, as the object of the decision.
        let whom = agent.as_deref().unwrap_or("your agents");
        let (pill, text) = match line.kind {
            AwayKind::Done => (Some(Pill::Done), words::briefing_sentence("turn", &who)),
            AwayKind::Failed if failed => (Some(Pill::Failed), format!("{who} failed.")),
            AwayKind::Failed => (Some(Pill::Failed), words::briefing_sentence("loop_guard", &who)),
            AwayKind::Stopped => (None, format!("{who} was stopped.")),
            AwayKind::Suggestion => match &agent {
                Some(agent) => {
                    (Some(Pill::Suggestion), words::briefing_sentence("proposal", agent))
                }
                None => (Some(Pill::Suggestion), words::AGENTS_SUGGEST.to_string()),
            },
            AwayKind::Question => (Some(Pill::Question), words::NOTICED_SOMETHING.to_string()),
            AwayKind::Decided if denied => {
                (Some(Pill::YouDecided), words::briefing_sentence("rejection", whom))
            }
            AwayKind::Decided => {
                (Some(Pill::YouDecided), words::briefing_sentence("approval", whom))
            }
            AwayKind::StoppedAll => (None, format!("{}.", words::ALL_AGENTS_STOPPED)),
            AwayKind::ResumedAll => (None, words::ALL_AGENTS_RESUMED.to_string()),
            // A kind a later kernel adds says what kind of thing happened, never more.
            _ => (None, words::briefing_sentence("", &who)),
        };
        Some(BriefingLine {
            seq: entry.seq,
            at,
            at_text: self.hh_mm(at),
            agent: who,
            pill,
            pill_text: pill.map(|pill| pill_words(pill).to_string()),
            text,
        })
    }

    /// The display name of the agent a ledger row names: its acting identity
    /// (`urn:adk:agent:{app}/{agent}`) or its target, as the roster names it; `None` for the
    /// kernel's own work, which no agent did.
    fn ledger_agent(&self, label: &str) -> Option<String> {
        if label == KERNEL_AGENT || label.is_empty() {
            return None;
        }
        let name = label.rsplit(['/', ':']).next().filter(|name| !name.is_empty()).unwrap_or(label);
        Some(self.agent(name).display_name)
    }
}

fn pill_words(pill: Pill) -> &'static str {
    match pill {
        Pill::Done => words::DONE,
        Pill::Failed => words::FAILED,
        Pill::WaitingForYou => words::WAITING_FOR_YOU,
        Pill::YouDecided => words::YOU_DECIDED,
        Pill::Suggestion => words::SUGGESTION,
        Pill::Question => words::QUESTION,
    }
}

/// The mode with its §8.1 words.
pub(crate) fn mode_view(mode: HarnessMode) -> ModeView {
    let (mode, label) = match mode {
        HarnessMode::ReadOnly => (Mode::JustLook, words::JUST_LOOK),
        HarnessMode::Plan => (Mode::Suggest, words::SUGGEST),
        HarnessMode::Auto => (Mode::GoAhead, words::GO_AHEAD),
        // Ask, and any mode a later harness adds, reads as the default.
        _ => (Mode::AskFirst, words::ASK_FIRST),
    };
    ModeView { mode, label: label.to_string(), status: words::mode_status(label) }
}

/// The harness mode a §8.1 mode word sets.
pub(crate) fn harness_mode(mode: Mode) -> HarnessMode {
    match mode {
        Mode::JustLook => HarnessMode::ReadOnly,
        Mode::Suggest => HarnessMode::Plan,
        Mode::AskFirst => HarnessMode::Ask,
        Mode::GoAhead => HarnessMode::Auto,
    }
}

pub(crate) fn spend_view(spend: SpendToday) -> SpendView {
    let text = match spend.limit {
        Some(limit) => words::spent_of(spend.spent, limit),
        None => words::spent_today(spend.spent),
    };
    SpendView { spent_micro_usd: spend.spent, limit_micro_usd: spend.limit, text }
}

/// The agent a request is from: the acting agent's name, else the application.
pub(crate) fn agent_name(request: &ApprovalRequest) -> String {
    request
        .acting
        .as_ref()
        .map(|acting| {
            let id = acting.agent.id.as_str();
            id.rsplit(['/', ':']).next().unwrap_or(id).to_string()
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| request.scope.app_name.clone())
}

/// The decision a held call links to in a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Link {
    /// The request's id.
    pub(crate) id: String,
    /// The request is still open.
    pub(crate) pending: bool,
}

/// How far apart a request's creation and the event that held its call may be and still be
/// one moment: the inbox writes the request just before or after the session writes the event.
const HOLD_SLACK_MS: i64 = 1_000;

/// Calls held closer together than this were held in one model turn and link in event order.
const ONE_TURN_MS: i64 = 500;

/// The calls `events` answer: every function response that is not a held call's placeholder,
/// by call ID.
pub(crate) fn answered_calls(events: &[Event]) -> HashSet<String> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, id, .. }
                if !adk_core::is_confirmation_pending(&function_response.response) =>
            {
                id.clone()
            }
            _ => None,
        })
        .collect()
}

/// The decision of each held call in a session's `events`, by the event's `seq` (its position
/// from 1), out of the session's `requests` (spec 3.3 §7.1).
///
/// A request with a fingerprint decides the calls held with that fingerprint. A request
/// served without one, as a kernel's approvals inbox serves them, decides a call of the same
/// tool by the same acting agent. Calls link in two passes over the session: first each call
/// to the request showing its exact arguments, then each call still unlinked to the earliest
/// request still free, since a route may show approvers redacted arguments — unless a later
/// call, held in another turn by the time that request was written, is the one it belongs to.
/// No request decides two calls, so a call whose own request is missing (unrouted, or not the
/// caller's to decide) links to none rather than to another call's, whether that call's
/// arguments are shown or redacted. A call held again keeps its request unless that request
/// was resolved before the call was held again.
pub(crate) fn links(events: &[Event], requests: &[ApprovalRequest]) -> HashMap<u64, Link> {
    let mut order: Vec<&ApprovalRequest> = requests.iter().collect();
    order.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    let holds: Vec<(u64, &ToolConfirmationRequest, chrono::DateTime<Utc>, String)> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            let held = event.actions.tool_confirmation.as_ref()?;
            Some((index as u64 + 1, held, event.timestamp, held.fingerprint()))
        })
        .collect();
    let decides = |request: &ApprovalRequest,
                   held: &ToolConfirmationRequest,
                   fingerprint: &str,
                   exact: bool| {
        match request.fingerprint.is_empty() {
            false => exact && request.fingerprint == fingerprint,
            true => {
                let agent = held.acting.as_deref().map(|acting| &acting.agent.id);
                request.tool_name == held.tool_name
                    && match (agent, &request.acting) {
                        (Some(agent), Some(acting)) => *agent == acting.agent.id,
                        _ => true,
                    }
                    && (!exact || request.args_redacted == held.args)
            }
        }
    };
    let mut taken = vec![false; order.len()];
    let mut linked: HashMap<u64, usize> = HashMap::new();
    for exact in [true, false] {
        let mut held_by: HashMap<&str, usize> = HashMap::new();
        for (hold, (seq, held, at, fingerprint)) in holds.iter().enumerate() {
            if let Some(&position) = linked.get(seq) {
                held_by.insert(fingerprint, position);
                continue;
            }
            // Whether `request` belongs to a later call of another turn: one still unlinked,
            // not a call held again under its open request, held by the time the request was
            // written.
            let later_owner = |request: &ApprovalRequest| {
                holds[hold + 1..].iter().enumerate().any(|(offset, (other, call, when, print))| {
                    let inherits = holds[..hold + 1 + offset].iter().any(|(earlier, _, _, p)| {
                        p == print
                            && linked.get(earlier).is_some_and(|&position| {
                                order[position].resolved_at.is_none_or(|at| at > *when)
                            })
                    });
                    !linked.contains_key(other)
                        && print != fingerprint
                        && !inherits
                        && *when > *at + chrono::Duration::milliseconds(ONE_TURN_MS)
                        && *when
                            <= request.created_at + chrono::Duration::milliseconds(HOLD_SLACK_MS)
                        && decides(request, call, print, false)
                })
            };
            let kept = held_by.get(fingerprint.as_str()).copied();
            let next = match kept {
                // Held again while its request was still open, as the event's time shows,
                // not the request's state now.
                Some(position)
                    if order[position].resolved_at.is_none_or(|resolved| resolved > *at) =>
                {
                    Some(position)
                }
                _ => (0..order.len())
                    .find(|&position| {
                        !taken[position]
                            && decides(order[position], held, fingerprint, exact)
                            && (exact || !later_owner(order[position]))
                    })
                    .or(kept),
            };
            if let Some(position) = next {
                taken[position] = true;
                held_by.insert(fingerprint, position);
                linked.insert(*seq, position);
            }
        }
    }
    linked
        .into_iter()
        .map(|(seq, position)| {
            let request = order[position];
            (seq, Link { id: request.id.clone(), pending: request.state == ApprovalState::Pending })
        })
        .collect()
}

/// The exact call, cut short past [`CALL_TEXT_LIMIT`] characters.
pub(crate) fn call_view(request: &ApprovalRequest) -> CallView {
    let full = call_text(&request.tool_name, &request.args_redacted);
    let truncated = full.chars().count() > CALL_TEXT_LIMIT;
    let text = if truncated {
        let mut cut: String = full.chars().take(CALL_TEXT_LIMIT).collect();
        cut.push('…');
        cut
    } else {
        full
    };
    CallView {
        tool: request.tool_name.clone(),
        args: request.args_redacted.clone(),
        text,
        truncated,
    }
}

/// The arguments the inbox shows are the call's own (`ApprovalRequest::shows_call_args`). A
/// source that serves no fingerprints, the approvals API, removes Edit itself where they are
/// not (`inbox::request_of`).
fn shows_own_arguments(request: &ApprovalRequest) -> bool {
    request.fingerprint.is_empty() || request.shows_call_args()
}

/// `tool(key: value, …)`, each value as JSON.
fn call_text(tool: &str, args: &Value) -> String {
    match args {
        Value::Object(map) => {
            let fields: Vec<String> =
                map.iter().map(|(key, value)| format!("{key}: {value}")).collect();
            format!("{tool}({})", fields.join(", "))
        }
        Value::Null => format!("{tool}()"),
        other => format!("{tool}({other})"),
    }
}

/// What will happen, in plain words, from the call: "Write file note.txt",
/// "Send email to [client email]" (spec 3.3 §7.1, §8 "Name the outcome"). A decision the
/// kernel itself asks is named by what it is about, never by the kernel's tool or its JSON:
/// an agenda item by its title.
pub(crate) fn heading(request: &ApprovalRequest) -> String {
    // A take over says what to type, in the words of the tool that asks (spec 3.3 §8.2).
    if request.tool_name == TAKE_OVER_TOOL && !request.summary.trim().is_empty() {
        return request.summary.clone();
    }
    if let Some(operation) = request.tool_name.strip_prefix(KERNEL_TOOL_PREFIX) {
        // The kernel's heading of a suggested agent, never the model's (spec 3.3 S-I13).
        if request.tool_name == adk_kernel::agents::AGENT_CREATE_TOOL_NAME {
            let name = request
                .args_redacted
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return crate::agents::words::add_an_agent_called(name);
        }
        if request.tool_name == AGENDA_TOOL_NAME {
            return match agenda_item(request) {
                Some(item) if !item.title.trim().is_empty() => sentence(&item.title),
                Some(item) if item.origin.is_question() => words::QUESTION.to_string(),
                Some(_) | None => words::SUGGESTION.to_string(),
            };
        }
        if operation == DRIVER_MOUNT_OPERATION
            && let Some(id) = request.args_redacted.get("id").and_then(Value::as_str)
        {
            return words::start_driver(id);
        }
        return humanize(operation);
    }
    describe_call(&request.tool_name, &request.args_redacted)
}

/// What a chat card says will happen: the tool in plain words, "Write file", "Send email" —
/// never its arguments, since a chat card leaves the machine (spec 3.3 §7.1, S-I5). The
/// kernel's own decisions say what kind they are: "Suggestion", "Question".
pub(crate) fn chat_heading(request: &ApprovalRequest) -> String {
    if request.tool_name == AGENDA_TOOL_NAME {
        return match agenda_item(request) {
            Some(item) if item.origin.is_question() => words::QUESTION.to_string(),
            Some(_) | None => words::SUGGESTION.to_string(),
        };
    }
    humanize(request.tool_name.strip_prefix(KERNEL_TOOL_PREFIX).unwrap_or(&request.tool_name))
}

/// The prefix of the inbox tools of decisions the kernel itself asks, not an agent's call:
/// `kernel:agenda`, `kernel:driver.mount`.
const KERNEL_TOOL_PREFIX: &str = "kernel:";

/// The operation of a driver mount a hosted definition asks for
/// (`adk_kernel::managed::DRIVER_MOUNT_TOOL_NAME`, behind the kernel's `managed` feature).
const DRIVER_MOUNT_OPERATION: &str = "driver.mount";

/// Cron weekdays: their number, their name, and their name in words.
const WEEKDAYS: [(&str, &str, &str); 8] = [
    ("0", "SUN", "Sunday"),
    ("1", "MON", "Monday"),
    ("2", "TUE", "Tuesday"),
    ("3", "WED", "Wednesday"),
    ("4", "THU", "Thursday"),
    ("5", "FRI", "Friday"),
    ("6", "SAT", "Saturday"),
    ("7", "SUN", "Sunday"),
];

/// The agenda item an agenda proposal carries (`adk_kernel::AGENDA_TOOL_NAME`): the item is
/// its arguments.
fn agenda_item(request: &ApprovalRequest) -> Option<AgendaItem> {
    (request.tool_name == AGENDA_TOOL_NAME)
        .then(|| serde_json::from_value(request.args_redacted.clone()).ok())
        .flatten()
}

/// The words of "allow once" and "deny" on a decision the kernel asks: a suggestion is done
/// or left for now, a question is right or forgotten (spec 3.3 §8.1, §8.2).
fn kernel_answer_words(request: &ApprovalRequest) -> Option<(&'static str, &'static str)> {
    if request.tool_name != AGENDA_TOOL_NAME {
        return None;
    }
    match agenda_item(request) {
        Some(item) if item.origin.is_question() => Some((words::THATS_RIGHT, words::FORGET_IT)),
        Some(_) | None => Some((words::DO_IT, words::NOT_NOW)),
    }
}

/// `text` on one line, starting with a capital: a title in sentence case (spec 3.3 §8).
fn sentence(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = line.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => line,
    }
}

/// The argument a heading names, short enough for one line.
fn primary_argument(args: &Value) -> Option<(&'static str, String)> {
    const KEYS: [&str; 9] =
        ["to", "path", "file", "url", "command", "recipient", "name", "query", "title"];
    let map = args.as_object()?;
    KEYS.iter().find_map(|key| {
        let value = map.get(*key)?.as_str()?.trim();
        if value.is_empty() {
            return None;
        }
        let mut short: String = value.chars().take(60).collect();
        if value.chars().count() > 60 {
            short.push('…');
        }
        Some((*key, short))
    })
}

/// Whether a call spends money: the payment tools `adk-payments` publishes, all named
/// `payments_*` (spec 3.1 §7.16), or a request whose route marks it.
pub(crate) fn spends_money(request: &ApprovalRequest) -> bool {
    request.tool_name.starts_with("payments_")
}

/// Whether a call can be taken back, from its name: messages can't be unsent; everything
/// else is shown as not undoable, since a surface cannot know better (spec 3.3 §7.1).
pub(crate) fn reversibility(request: &ApprovalRequest) -> Reversibility {
    let name = request.tool_name.to_lowercase();
    let sends = ["send", "email", "mail", "message", "sms", "post", "reply", "notify", "publish"];
    if sends.iter().any(|word| name.contains(word)) {
        Reversibility::CantBeUnsent
    } else {
        Reversibility::CantBeUndone
    }
}

/// A short line for a tool call in a thread: "Read file timesheet-october.csv".
fn describe_call(tool: &str, args: &Value) -> String {
    let mut text = humanize(tool);
    if let Some((key, value)) = primary_argument(args) {
        text.push(' ');
        if key == "to" {
            text.push_str("to ");
        }
        text.push_str(&value);
    }
    text
}

fn response_failed(response: &Value) -> bool {
    response.get("error").is_some()
        || response
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| matches!(status, "error" | "failed" | "denied" | "not_run"))
}

/// `send_email` → "Send email"; `chiefOfStaff` → "Chief of staff".
pub(crate) fn humanize(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for c in name.chars() {
        if matches!(c, '_' | '-' | ' ' | '.' | ':' | '/') {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if c.is_uppercase() && previous_lower && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
        current.extend(c.to_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    let mut text = words.join(" ");
    if let Some(first) = text.get(..1) {
        let upper = first.to_uppercase();
        text.replace_range(..1, &upper);
    }
    if text.is_empty() { name.to_string() } else { text }
}

/// A monogram in the agent's own colour (spec 3.3 §9).
fn avatar(display_name: &str) -> Avatar {
    const COLORS: [&str; 8] =
        ["#C03A00", "#15803D", "#1D4ED8", "#7C3AED", "#475569", "#B45309", "#0E7490", "#BE185D"];
    let monogram: String = display_name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    let hash = display_name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    let index = usize::try_from(hash % COLORS.len() as u64).unwrap_or(0);
    Avatar { monogram, color: COLORS[index].to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{ApprovalPolicy, ApprovalScope, ToolConfirmationRequest};
    use chrono::{Duration, NaiveTime};
    use serde_json::json;

    fn time(hh: u32, mm: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(hh, mm, 0).unwrap()
    }

    fn minutes(minutes: i64) -> Duration {
        Duration::minutes(minutes)
    }

    fn request(tool: &str, args: Value) -> ApprovalRequest {
        let call = ToolConfirmationRequest {
            tool_name: tool.into(),
            function_call_id: Some("c1".into()),
            args,
            origin: None,
            acting: None,
        };
        ApprovalRequest::new(
            "apr_1",
            ApprovalScope::new("app", "ana", "s1"),
            &call,
            ApprovalPolicy::new([]),
        )
    }

    #[test]
    fn headings_name_the_outcome() {
        assert_eq!(
            heading(&request("send_email", json!({ "to": "[client email]" }))),
            "Send email to [client email]"
        );
        assert_eq!(
            heading(&request("write_file", json!({ "path": "note.txt", "content": "x" }))),
            "Write file note.txt"
        );
        assert_eq!(humanize("chiefOfStaff"), "Chief of staff");
    }

    /// A proposal of the kernel's agenda, as `Agenda::submit` puts it to the inbox.
    fn agenda(item: &AgendaItem) -> ApprovalRequest {
        let args = serde_json::to_value(item).unwrap();
        let request = request(AGENDA_TOOL_NAME, args.clone())
            .with_redacted_args(args)
            .with_summary(format!("Agenda follow-up: {}", item.title));
        if item.origin.is_question() {
            request.with_allowed_actions([ApprovalAction::Approve, ApprovalAction::Reject])
        } else {
            request.with_allowed_actions([
                ApprovalAction::Approve,
                ApprovalAction::Edit,
                ApprovalAction::Reject,
            ])
        }
    }

    fn item(title: &str, origin: AgendaOrigin) -> AgendaItem {
        AgendaItem::new(
            "item-1",
            adk_core::principal::SubjectRef::human("ana"),
            title,
            "worth doing",
            origin,
            Utc.with_ymd_and_hms(2026, 10, 25, 7, 3, 0).unwrap(),
        )
    }

    /// The owner's 07:03 demo: a decision the kernel asks reads in plain words — who suggests
    /// it, what it does, when it runs — and never as the kernel's tool or its JSON (spec 3.3
    /// §7.1, §8).
    #[test]
    fn kernel_decisions_read_in_plain_words() {
        let mut projector = Projector::new(FixedOffset::east_opt(3 * 3600).unwrap());
        projector.names.insert("app".into(), "Finance".into());
        let now = Utc.with_ymd_and_hms(2026, 10, 25, 4, 3, 0).unwrap();
        let agent = AgendaOrigin::Agent { pid: "run-7".into(), confidence: 1.0 };
        let cron =
            |expr: &str| Wake::Cron { expr: expr.into(), missed: adk_core::MissedFirePolicy::Skip };
        let cases = [
            (
                item("check the news every 5 minutes", agent.clone())
                    .with_when(cron("*/5 * * * *")),
                "Check the news every 5 minutes",
                "Finance suggests this. Once you say yes, it runs every 5 minutes.",
            ),
            // 04:00 UTC is 07:00 for a person at UTC+3.
            (
                item("Read the overnight mail", agent.clone()).with_when(cron("0 0 4 * * MON-FRI")),
                "Read the overnight mail",
                "Finance suggests this. Once you say yes, it runs every weekday at 07:00.",
            ),
            (
                item("Book the dentist", agent.clone())
                    .with_when(Wake::At(Utc.with_ymd_and_hms(2026, 10, 26, 6, 30, 0).unwrap())),
                "Book the dentist",
                "Finance suggests this. Once you say yes, it runs tomorrow at 09:30.",
            ),
            (
                item("Tidy the notes", AgendaOrigin::Consolidation),
                "Tidy the notes",
                "Your agents suggest this. Once you say yes, it runs right away.",
            ),
            (
                item(
                    "You prefer morning meetings",
                    AgendaOrigin::Observation { id: "obs-1".into() },
                ),
                "You prefer morning meetings",
                "Your agents noticed something. Is that right?",
            ),
            (
                item("Archive", agent).with_when(cron("15 3 1 * *")),
                "Archive",
                "Finance suggests this. Once you say yes, it runs on the schedule 15 3 1 * *.",
            ),
        ];
        let me = Caller::human("ana", SurfaceKind::Web, crate::origin::Origin::Pointer);
        for (item, heading, summary) in cases {
            let card = projector.card(&agenda(&item), &me, None, None, now, &CardHint::new());
            assert_eq!((card.heading.as_str(), card.summary.as_deref()), (heading, Some(summary)));
            for text in [card.heading.as_str(), summary] {
                assert!(!text.contains("kernel") && !text.contains('{'), "{text}");
            }
            let labels: Vec<&str> = card.allowed.options.iter().map(|o| o.label.as_str()).collect();
            let expected: &[&str] = if item.origin.is_question() {
                &["That's right", "Forget it"]
            } else {
                &["Do it", "Edit", "Not now"]
            };
            assert_eq!(labels, expected, "{heading}");
        }

        // A driver mount names what it starts; any other kernel decision its operation.
        let mount = request(
            "kernel:driver.mount",
            json!({ "id": "email", "commandLine": "mcp-email --inbox", "partitions": ["default"] }),
        );
        assert_eq!(heading(&mount), "Start email for your agents");
        assert_eq!(
            projector.kernel_summary(&mount, now).as_deref(),
            Some("It runs mcp-email --inbox on the computer your agents run on.")
        );
        let other = request("kernel:quota.raise", json!({ "partition": "default" }));
        assert_eq!(
            (heading(&other), projector.kernel_summary(&other, now)),
            ("Quota raise".to_string(), None)
        );
        // An agenda request whose item cannot be read still names no internals.
        let unreadable = request(AGENDA_TOOL_NAME, json!({ "planHash": "sha256:00" }));
        assert_eq!(heading(&unreadable), "Suggestion");
        assert_eq!(chat_heading(&unreadable), "Suggestion");
    }

    #[test]
    fn long_calls_are_truncated_and_flagged() {
        let long = request("write_file", json!({ "content": "x".repeat(5_000) }));
        let view = call_view(&long);
        assert!(view.truncated);
        assert!(view.text.chars().count() <= CALL_TEXT_LIMIT + 1);
        assert!(!call_view(&request("write_file", json!({ "path": "a" }))).truncated);
    }

    #[test]
    fn quiet_time_in_the_timeline() {
        let mut projector = Projector::new(FixedOffset::east_opt(0).unwrap());
        projector.quiet = Some(QuietHours::new(time(22, 0), time(7, 0)).with_utc_offset_minutes(0));
        let asked = Utc.with_ymd_and_hms(2026, 10, 23, 18, 0, 0).unwrap();
        assert!(!projector.waited_through_quiet(asked, asked + minutes(60)));
        assert!(projector.waited_through_quiet(asked, asked + minutes(13 * 60 + 30)));
        let view = projector.quiet_view(asked).unwrap();
        assert_eq!(view.text, "Quiet from 22:00");
        assert_eq!(
            projector.quiet_view(asked + minutes(6 * 60)).unwrap().text,
            "Quiet until 07:00"
        );
    }

    #[test]
    fn agent_rows_say_what_each_agent_is_doing() {
        let projector = Projector::new(FixedOffset::east_opt(0).unwrap());
        let state = |state, waiting_on_decision, wake_at, suspended| AgentState {
            name: "chief_of_staff".into(),
            session_id: None,
            state,
            waiting_on_decision,
            wake_at,
            suspended,
        };
        let seven = Some(Utc.with_ymd_and_hms(2026, 10, 24, 7, 0, 0).unwrap());
        let words = |row: AgentRow| (row.agent.display_name, row.status_text);
        assert_eq!(
            words(projector.agent_row(&state(RunState::Running, false, None, false))),
            ("Chief of staff".into(), "Working".into())
        );
        assert_eq!(
            projector.agent_row(&state(RunState::Waiting, true, None, false)).status_text,
            "Waiting for you"
        );
        assert_eq!(
            projector.agent_row(&state(RunState::Waiting, false, seven, false)).status_text,
            "Asleep until 07:00"
        );
        assert_eq!(
            projector.agent_row(&state(RunState::Waiting, false, seven, true)).status_text,
            "Asleep"
        );
        assert_eq!(
            projector.agent_row(&state(RunState::Completed, false, None, false)).status_text,
            "Done"
        );
        assert_eq!(
            projector.agent_row(&state(RunState::Failed, false, None, false)).status_text,
            "Failed"
        );
    }

    #[test]
    fn briefing_lines_come_from_ledger_rows() {
        let mut projector = Projector::new(FixedOffset::east_opt(3 * 3600).unwrap());
        projector.names.insert("adk-rust".into(), "ADK-Rust".into());
        let row = |kind: &str, agent: &str, effect: Option<&str>| -> LedgerEntry {
            serde_json::from_value(json!({
                "seq": 7,
                "ts": "2026-10-23T23:05:00+03:00",
                "partition": "default",
                "agent": agent,
                "kind": kind,
                "effect": effect,
            }))
            .unwrap()
        };
        let line = projector.briefing_line(&row("proposal", "kernel", Some("proposed"))).unwrap();
        assert_eq!(
            (line.at_text.as_str(), line.pill_text.as_deref(), line.seq),
            ("23:05", Some("Suggestion"), 7)
        );
        assert_eq!(line.text, "Your agents suggest something. Nothing happens until you say so.");
        let agent = "urn:adk:agent:adk-rust/adk-rust";
        let decided = projector.briefing_line(&row("decided", agent, Some("approve"))).unwrap();
        assert_eq!(
            (decided.pill_text.as_deref(), decided.text.as_str()),
            (Some("You decided"), "You allowed ADK-Rust to go ahead.")
        );
        // The person's answer recorded by the harness is the decision's row.
        let said_no = projector.briefing_line(&row("rejection", agent, None)).unwrap();
        assert_eq!(said_no.text, "You said no to ADK-Rust.");
        let question = projector.briefing_line(&row("observation", "kernel", None)).unwrap();
        assert_eq!(question.text, "Your agents noticed something. Is that right?");
        // The kernel's housekeeping and an agent's steps that are no event of their own make no
        // line.
        for (kind, agent, effect) in [
            ("wake", "kernel", Some("woke")),
            ("process", "kernel", Some("completed")),
            ("process", agent, Some("running")),
            ("spend", agent, None),
            ("intent", agent, None),
            ("tool_call", agent, None),
            ("proposal", "kernel", Some("running")),
        ] {
            assert_eq!(
                projector.briefing_line(&row(kind, agent, effect)),
                None,
                "{kind} {effect:?}"
            );
        }
    }
}
