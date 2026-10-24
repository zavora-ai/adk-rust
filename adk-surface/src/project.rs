//! Projection: inbox entries, session events, process states and ledger rows become cards,
//! thread items, agent rows, status and briefing lines, once, in Rust (spec 3.3 §6, §7.1, §8).

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

use adk_core::{ApprovalAction, ApprovalRequest, ApprovalState, Event, Part, RunState};
use adk_harness::HarnessMode;
use adk_kernel::QuietHours;
use adk_kernel::proc::LedgerEntry;
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use serde_json::Value;

use crate::answers::AnswerRecord;
use crate::model::{
    AgentRef, AgentRow, AgentStatus, Avatar, BriefingLine, CallView, CardStatus, Decided,
    DecidedAnswer, DecisionCard, FeedEntry, Mode, ModeView, Pill, QuietView, Reversibility,
    Speaker, SpendView, ThreadItem, TimelineStep,
};
use crate::origin::{Caller, SurfaceKind};
use crate::rules::{self, DecisionFacts};
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
    pub(crate) reversibility: Classify<Reversibility>,
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
            reversibility: Arc::new(reversibility),
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

    /// The facts the §7.2 rules read.
    pub(crate) fn facts(
        &self,
        request: &ApprovalRequest,
        always_label: Option<String>,
    ) -> DecisionFacts {
        DecisionFacts {
            risk: request.risk.into(),
            money: (self.money)(request),
            truncated: call_view(request).truncated,
            allows: request.allowed_actions.clone(),
            always_label,
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
        Some(Decided { by, by_you, via, at, at_text, answer: answer_kind, text })
    }

    /// The card of `request` as `caller` sees it (spec 3.3 §7.1).
    pub(crate) fn card(
        &self,
        request: &ApprovalRequest,
        caller: &Caller,
        always_label: Option<String>,
        answer: Option<&AnswerRecord>,
        now: DateTime<Utc>,
    ) -> DecisionCard {
        let facts = self.facts(request, always_label);
        // A chat card leaves the machine through someone else's servers: it carries the
        // agent, the risk and a short summary, never the call's arguments (S-I5).
        let (heading, call) = if rules::row(caller) == rules::Row::Chat {
            let full = call_view(request);
            let call = CallView {
                tool: full.tool.clone(),
                args: Value::Null,
                text: full.tool,
                truncated: full.truncated,
            };
            (humanize(&request.tool_name), call)
        } else {
            ((self.heading)(request), call_view(request))
        };
        let decided = self.decided(request, caller, answer);
        let mut allowed = rules::allowed_answers(caller, &facts);
        if decided.is_some() {
            allowed.options.clear();
            allowed.open_in_home = false;
            allowed.cancel_window_secs = None;
            allowed.note = None;
        }
        let since = self.hh_mm(request.created_at);
        let mut timeline = vec![words::asked_at(&since)];
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
        let reversibility = (self.reversibility)(request);
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
            call,
            timeline,
            allowed,
            risk: facts.risk,
            risk_text: facts.risk.words().to_string(),
            reversibility,
            reversibility_text: match reversibility {
                Reversibility::CantBeUnsent => words::CANT_BE_UNSENT,
                Reversibility::CantBeUndone => words::CANT_BE_UNDONE,
            }
            .to_string(),
            money: facts.money,
            also_shown: words::also_shown(&also),
            decided,
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

    /// The event at `seq`, its held call linked to the decision [`links`] gives that `seq`.
    pub(crate) fn entry(&self, seq: u64, event: &Event, links: &HashMap<u64, String>) -> FeedEntry {
        FeedEntry {
            seq,
            at: event.timestamp,
            at_text: self.hh_mm(event.timestamp),
            items: self.items(event, links.get(&seq).cloned()),
        }
    }

    fn items(&self, event: &Event, decision_id: Option<String>) -> Vec<ThreadItem> {
        let mut items = Vec::new();
        let from_person = event.author == "user";
        if let Some(content) = &event.llm_response.content {
            let mut text = String::new();
            for part in &content.parts {
                match part {
                    Part::Text { text: chunk } if !event.llm_response.partial => {
                        text.push_str(chunk);
                    }
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
                    Part::FunctionResponse { function_response, id, .. } => {
                        let ok = !response_failed(&function_response.response);
                        items.push(ThreadItem::Result {
                            tool: function_response.name.clone(),
                            call_id: id.clone(),
                            ok,
                            text: if ok { words::DONE } else { words::FAILED }.to_string(),
                        });
                    }
                    _ => {}
                }
            }
            let text = text.trim();
            // The event that holds a call carries the runtime's own notice, not the agent's
            // words: its Decision item says it plainly (§8).
            if !text.is_empty() && event.actions.tool_confirmation.is_none() {
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
            items.push(ThreadItem::Decision {
                decision_id,
                tool: held.tool_name.clone(),
                text: words::WAITING_FOR_YOU.to_string(),
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
        }
    }

    pub(crate) fn briefing_line(&self, entry: &LedgerEntry) -> BriefingLine {
        let at = entry.utc();
        let agent = self.agent(&entry.agent).display_name;
        let pill = match entry.kind.as_str() {
            "approval" | "rejection" | "decided" => Some(Pill::YouDecided),
            "held" => Some(Pill::WaitingForYou),
            "proposal" => Some(Pill::Suggestion),
            "observation" => Some(Pill::Question),
            "loop_guard" | "denial" => Some(Pill::Failed),
            "turn" | "rule_hit" => Some(Pill::Done),
            "process" => match entry.meta.get("state").and_then(Value::as_str) {
                Some("failed") => Some(Pill::Failed),
                Some("completed") => Some(Pill::Done),
                _ => None,
            },
            _ => None,
        };
        let text = match (entry.kind.as_str(), pill) {
            ("process", Some(Pill::Failed)) => format!("{agent} failed."),
            ("process", Some(Pill::Done)) => format!("{agent} finished."),
            ("process", _) => format!("{agent} changed what it is doing."),
            (kind, _) => words::briefing_sentence(kind, &agent),
        };
        BriefingLine {
            seq: entry.seq,
            at,
            at_text: self.hh_mm(at),
            agent,
            pill,
            pill_text: pill.map(|pill| pill_words(pill).to_string()),
            text,
        }
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

/// The decision of each held call in a session's `events`, by the event's `seq` (its position
/// from 1), out of the session's `requests` (spec 3.3 §7.1).
///
/// A request with a fingerprint decides the calls held with that fingerprint. A request
/// served without one, as a kernel's approvals inbox serves them, decides a call of the same
/// tool by the same acting agent: the one showing the call's arguments, else the earliest
/// one, since a route may show approvers redacted arguments. No request decides two calls,
/// and a call held again keeps its request until a decided request has a newer one.
pub(crate) fn links(events: &[Event], requests: &[ApprovalRequest]) -> HashMap<u64, String> {
    let mut order: Vec<&ApprovalRequest> = requests.iter().collect();
    order.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    let mut taken = vec![false; order.len()];
    let mut held_by: HashMap<String, usize> = HashMap::new();
    let mut links = HashMap::new();
    for (index, event) in events.iter().enumerate() {
        let Some(held) = &event.actions.tool_confirmation else { continue };
        let fingerprint = held.fingerprint();
        let agent = held.acting.as_deref().map(|acting| &acting.agent.id);
        let decides = |request: &ApprovalRequest| match request.fingerprint.is_empty() {
            false => request.fingerprint == fingerprint,
            true => {
                request.tool_name == held.tool_name
                    && match (agent, &request.acting) {
                        (Some(agent), Some(acting)) => *agent == acting.agent.id,
                        _ => true,
                    }
            }
        };
        let free = |same_args: bool| {
            (0..order.len()).find(|&position| {
                let request = order[position];
                !taken[position]
                    && decides(request)
                    && (!same_args || request.args_redacted == held.args)
            })
        };
        let kept = held_by.get(&fingerprint).copied();
        let next = match kept {
            Some(position) if order[position].state == ApprovalState::Pending => Some(position),
            _ => free(true).or_else(|| free(false)).or(kept),
        };
        if let Some(position) = next {
            taken[position] = true;
            held_by.insert(fingerprint, position);
            links.insert(index as u64 + 1, order[position].id.clone());
        }
    }
    links
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
/// "Send email to [client email]" (spec 3.3 §7.1, §8 "Name the outcome").
pub(crate) fn heading(request: &ApprovalRequest) -> String {
    describe_call(&request.tool_name, &request.args_redacted)
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
        if c == '_' || c == '-' || c == ' ' || c == '.' {
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
        let projector = Projector::new(FixedOffset::east_opt(3 * 3600).unwrap());
        let row = |kind: &str| -> LedgerEntry {
            serde_json::from_value(json!({
                "seq": 7,
                "ts": "2026-10-23T23:05:00+03:00",
                "partition": "default",
                "agent": "finance",
                "kind": kind,
            }))
            .unwrap()
        };
        let line = projector.briefing_line(&row("proposal"));
        assert_eq!(
            (line.at_text.as_str(), line.pill_text.as_deref()),
            ("23:05", Some("Suggestion"))
        );
        assert_eq!(line.text, "Finance suggests something. Nothing happens until you say so.");
        assert_eq!(line.seq, 7);
        assert_eq!(
            projector.briefing_line(&row("approval")).pill_text.as_deref(),
            Some("You decided")
        );
        assert_eq!(projector.briefing_line(&row("observation")).text, "Is that right?");
    }
}
