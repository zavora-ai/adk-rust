//! Auto-compaction during a run.
//!
//! [`IntraInvocationCompactor`] is the runner's [`HistoryCompactor`]: before each model call
//! an agent asks it to compact its history, and it summarizes all but the most recent entries
//! once the request passes the [`IntraCompactionConfig`] threshold. By default the summary is
//! written by the model the agent is calling ([`LlmEventSummarizer`]); a
//! [`BaseEventsSummarizer`] or, with the `context-compaction` feature, a
//! `CompactionStrategy` (`compaction::CompactionStrategy`) can take its place.
//!
//! The runner installs one per run over the run's session, so each compaction is also
//! recorded as a compaction event and later runs start from the summary.

use adk_core::history::{balanced_content_cut, held_call_content_limit};
use adk_core::intra_compaction::{
    CompactedHistory, CompactionTrigger, HistoryCompactionRequest, HistoryCompactor,
    IntraCompactionConfig, compaction_summary, estimate_content_tokens, estimate_tokens,
};
use adk_core::{
    BaseEventsSummarizer, Content, Event, EventCompaction, InvocationContext, Llm,
    LlmEventSummarizer, Part, Result,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::context::MutableSession;

/// The runner's auto-compaction: summarizes an agent's history once a request nears the
/// model's context window.
///
/// Before each model call, `LlmAgent` and `CodeActAgent` pass their history to the run's
/// [`HistoryCompactor`]. Once the estimated request, or the prompt size the provider reported
/// for the agent's previous call, passes [`IntraCompactionConfig::threshold_for`] the model's
/// [`context_window`](Llm::context_window), this compactor summarizes all but the most recent
/// entries — at most `overlap_event_count` of them, within half the threshold — and replaces
/// them with one
/// [summary notice](adk_core::intra_compaction::compaction_summary).
///
/// The cut never separates a function response from its call, a call held for confirmation
/// and everything after it stay, the summary call counts against the run's budget and spend
/// ledger, and a summary that is not smaller than what it replaces is not used. A summarizer
/// that fails leaves the history as it is.
///
/// [`maybe_compact`](Self::maybe_compact) applies the same rules to a list of session events.
///
/// # Example
///
/// ```rust
/// use adk_core::IntraCompactionConfig;
/// use adk_runner::IntraInvocationCompactor;
///
/// // Summarizes with the model each agent calls, past 50,000 estimated tokens.
/// let compactor = IntraInvocationCompactor::with_model_summary(
///     IntraCompactionConfig::at_tokens(50_000),
/// );
/// assert_eq!(compactor.config().threshold_for(None), 50_000);
/// ```
pub struct IntraInvocationCompactor {
    config: IntraCompactionConfig,
    /// `None` summarizes with the model the agent is calling.
    summarizer: Option<Arc<dyn BaseEventsSummarizer>>,
    #[cfg(feature = "context-compaction")]
    strategy: Option<Arc<crate::compaction::CompactionConfig>>,
    /// Track whether compaction already ran this LLM call cycle.
    compacted_this_cycle: AtomicBool,
}

impl std::fmt::Debug for IntraInvocationCompactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntraInvocationCompactor")
            .field("config", &self.config)
            .field("summarizer", &self.summarizer.as_ref().map(|_| "<dyn BaseEventsSummarizer>"))
            .finish_non_exhaustive()
    }
}

impl IntraInvocationCompactor {
    /// A compactor that summarizes with `summarizer`.
    pub fn new(config: IntraCompactionConfig, summarizer: Arc<dyn BaseEventsSummarizer>) -> Self {
        Self {
            config,
            summarizer: Some(summarizer),
            #[cfg(feature = "context-compaction")]
            strategy: None,
            compacted_this_cycle: AtomicBool::new(false),
        }
    }

    /// A compactor that summarizes with the model the agent is calling, through
    /// [`LlmEventSummarizer`]. This is the runner's default.
    pub fn with_model_summary(config: IntraCompactionConfig) -> Self {
        Self {
            config,
            summarizer: None,
            #[cfg(feature = "context-compaction")]
            strategy: None,
            compacted_this_cycle: AtomicBool::new(false),
        }
    }

    /// A compactor that shortens a history with `config.strategy` once a request passes
    /// `config.context_budget` estimated tokens, applying it up to `config.max_retries`
    /// times. `overlap_event_count` and `chars_per_token` come from `base`.
    #[cfg(feature = "context-compaction")]
    pub fn with_strategy(
        base: IntraCompactionConfig,
        config: Arc<crate::compaction::CompactionConfig>,
    ) -> Self {
        Self {
            config: IntraCompactionConfig {
                token_threshold: Some(config.context_budget as u64),
                ..base
            },
            summarizer: None,
            strategy: Some(config),
            compacted_this_cycle: AtomicBool::new(false),
        }
    }

    /// The threshold and overlap this compactor applies.
    pub fn config(&self) -> &IntraCompactionConfig {
        &self.config
    }

    /// Check if compaction is needed and perform it if so.
    ///
    /// Returns `Some(compacted_events)` if compaction was triggered, or `None`
    /// if no compaction was needed (below threshold or already compacted this cycle),
    /// or the compactor summarizes with the agent's model, which a list of events does not
    /// name.
    ///
    /// On summarizer error, logs a warning and returns `None` (uncompacted history
    /// is used).
    ///
    /// # Errors
    ///
    /// Returns none today; a failed summary leaves the events as they are.
    pub async fn maybe_compact(&self, events: &[Event]) -> Result<Option<Vec<Event>>> {
        let Some(summarizer) = self.summarizer.as_ref() else {
            return Ok(None);
        };
        // Guard: at most once per cycle
        if self
            .compacted_this_cycle
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(None);
        }

        let estimated = estimate_tokens(events, self.config.chars_per_token);
        if estimated <= self.config.threshold_for(None) {
            // Below threshold — reset the guard so a future call in the same
            // cycle can still trigger if tokens grow.
            self.compacted_this_cycle.store(false, Ordering::SeqCst);
            return Ok(None);
        }

        // Determine which events to summarize vs. preserve. A held call and everything after
        // it stay, so its approval can still resume it, and the cut never separates a function
        // response from its call.
        let overlap = self.config.overlap_event_count.min(events.len());
        let summarize_end = adk_core::history::balanced_cut(
            events,
            adk_core::held_call_compaction_limit(events, events.len().saturating_sub(overlap)),
        );

        if summarize_end == 0 {
            // All events are in the overlap window — nothing to summarize
            self.compacted_this_cycle.store(false, Ordering::SeqCst);
            return Ok(None);
        }

        let events_to_summarize = &events[..summarize_end];
        let overlap_events = &events[summarize_end..];

        // Call the summarizer — on error, log and return uncompacted
        match summarizer.summarize_events(events_to_summarize).await {
            Ok(Some(summary_event)) => {
                // A summary that is not smaller than what it replaces would not shrink the
                // request.
                let summary_tokens = estimate_tokens(
                    std::slice::from_ref(&summary_event),
                    self.config.chars_per_token,
                );
                let replaced_tokens =
                    estimate_tokens(events_to_summarize, self.config.chars_per_token);
                if summary_tokens >= replaced_tokens {
                    tracing::info!(
                        summary.tokens = summary_tokens,
                        replaced.tokens = replaced_tokens,
                        "intra-invocation summary is not smaller than the history it replaces; keeping the history"
                    );
                    return Ok(None);
                }
                let mut compacted = Vec::with_capacity(1 + overlap_events.len());
                compacted.push(summary_event);
                compacted.extend_from_slice(overlap_events);
                Ok(Some(compacted))
            }
            Ok(None) => {
                // Summarizer returned None (e.g., empty input) — no compaction
                Ok(None)
            }
            Err(e) => {
                // A summarizer's model error can echo the history it was given.
                tracing::warn!(
                    error = %e.diagnostics(),
                    "intra-invocation compaction failed, continuing with uncompacted history"
                );
                Ok(None)
            }
        }
    }

    /// Reset the per-cycle guard. Call this at the start of each LLM call.
    pub fn reset_cycle(&self) {
        self.compacted_this_cycle.store(false, Ordering::SeqCst);
    }

    /// The history compaction both hooks share; `session` records it as a compaction event.
    async fn compact_history(
        &self,
        ctx: Arc<dyn InvocationContext>,
        model: Arc<dyn Llm>,
        request: HistoryCompactionRequest<'_>,
        session: Option<&MutableSession>,
    ) -> Outcome {
        let chars_per_token = self.config.chars_per_token;
        let threshold = self.config.threshold_for(model.context_window());
        let estimated = estimate_content_tokens(request.preamble, chars_per_token)
            + estimate_content_tokens(request.history, chars_per_token);
        let size = estimated.max(request.reported_prompt_tokens.unwrap_or(0));
        if request.trigger == CompactionTrigger::BeforeModelCall && size <= threshold {
            return Outcome::BelowThreshold;
        }
        tracing::info!(
            agent.name = %ctx.agent_name(),
            history.entries = request.history.len(),
            history.tokens = size,
            threshold,
            trigger = ?request.trigger,
            "compacting the agent's history"
        );
        // The summary call counts against the run's budget and spend ledger.
        let scope =
            adk_core::ModelCallScope::for_run(ctx.run_config(), ctx.app_name(), ctx.agent_name());
        #[cfg(feature = "context-compaction")]
        if let Some(strategy) = &self.strategy {
            return scope
                .within(self.compact_with_strategy(ctx.as_ref(), strategy, request.history))
                .await;
        }
        scope.within(self.summarize(ctx.as_ref(), model, request.history, threshold, session)).await
    }

    /// Summarizes all but the most recent entries of `history`: at most `overlap_event_count`
    /// of them, and no more than half of `threshold`, so the compacted history has room to
    /// grow.
    async fn summarize(
        &self,
        ctx: &dyn InvocationContext,
        model: Arc<dyn Llm>,
        history: &[Content],
        threshold: u64,
        session: Option<&MutableSession>,
    ) -> Outcome {
        let chars_per_token = self.config.chars_per_token;
        let overlap = self.config.overlap_event_count.min(history.len());
        let mut within_budget = history.len();
        let mut kept_tokens = 0;
        for (index, content) in history.iter().enumerate().rev() {
            kept_tokens += estimate_content_tokens(std::slice::from_ref(content), chars_per_token);
            // The latest entry stays whatever its size.
            if kept_tokens > threshold / 2 && index + 1 < history.len() {
                break;
            }
            within_budget = index;
        }
        let cut = balanced_content_cut(
            history,
            held_call_content_limit(
                history,
                history.len().saturating_sub(overlap).max(within_budget),
            ),
        );
        // A cut whose first kept entry the session can place is recorded there too; one in
        // a parallel branch is not, since a session's compaction applies to every branch.
        let recorded = session
            .filter(|_| ctx.branch().is_empty())
            .and_then(|session| session.read_events(|events| session_cut(events, history, cut)));
        let cut = recorded.as_ref().map_or(cut, |recorded| recorded.cut);
        if cut == 0 {
            return Outcome::NothingToCompact;
        }
        let summarized = &history[..cut];
        let events = as_events(ctx, summarized);
        let summarizer = self
            .summarizer
            .clone()
            .unwrap_or_else(|| Arc::new(LlmEventSummarizer::new(model)) as _);
        let text = match summarizer.summarize_events(&events).await {
            Ok(Some(event)) => summary_text(&event),
            Ok(None) => return Outcome::Kept,
            Err(error) => {
                // A summarizer's model error can echo the history it was given.
                tracing::warn!(
                    error = %error.diagnostics(),
                    "history compaction failed; continuing with the history as it is"
                );
                return Outcome::Kept;
            }
        };
        if text.trim().is_empty() {
            return Outcome::Kept;
        }
        let summary = compaction_summary(&text);
        let summary_tokens =
            estimate_content_tokens(std::slice::from_ref(&summary), chars_per_token);
        let replaced_tokens = estimate_content_tokens(summarized, chars_per_token);
        if summary_tokens >= replaced_tokens {
            tracing::info!(
                summary.tokens = summary_tokens,
                replaced.tokens = replaced_tokens,
                "history summary is not smaller than the history it replaces; keeping the history"
            );
            return Outcome::Kept;
        }
        let event = recorded.map(|recorded| {
            let mut event = Event::new(ctx.invocation_id());
            event.author = ctx.agent_name().to_string();
            event.branch = ctx.branch().to_string();
            event.actions.compaction =
                Some(EventCompaction { compacted_content: summary.clone(), ..recorded.span });
            event
        });
        tracing::info!(
            agent.name = %ctx.agent_name(),
            summarized.entries = cut,
            kept.entries = history.len() - cut,
            summary.tokens = summary_tokens,
            replaced.tokens = replaced_tokens,
            recorded = event.is_some(),
            "history compacted"
        );
        let history = std::iter::once(summary).chain(without_thinking(&history[cut..])).collect();
        Outcome::Compacted(Box::new(CompactedHistory { history, event }))
    }

    /// Shortens `history` with a compaction strategy, keeping a held call and everything
    /// after it.
    #[cfg(feature = "context-compaction")]
    async fn compact_with_strategy(
        &self,
        ctx: &dyn InvocationContext,
        config: &crate::compaction::CompactionConfig,
        history: &[Content],
    ) -> Outcome {
        let held_from = held_call_content_limit(history, history.len());
        let events = as_events(ctx, &history[..held_from]);
        let ids: HashMap<String, usize> =
            events.iter().enumerate().map(|(index, event)| (event.id.clone(), index)).collect();
        let compacted = match crate::compaction::apply_compaction_with_retry(config, events).await {
            Ok(compacted) => compacted,
            Err(error) => {
                tracing::warn!(
                    error = %error.diagnostics(),
                    "history compaction failed; continuing with the history as it is"
                );
                return Outcome::Kept;
            }
        };
        // The strategy keeps a tail of the history as it was; whatever comes before that
        // tail stands in for the rest.
        let mut tail_from = compacted.len();
        let mut next = held_from;
        while tail_from > 0
            && next > 0
            && ids.get(&compacted[tail_from - 1].id) == Some(&(next - 1))
        {
            tail_from -= 1;
            next -= 1;
        }
        let mut shortened: Vec<Content> = compacted[..tail_from]
            .iter()
            .filter_map(|event| {
                let content = event.content()?;
                // A summary the strategy wrote reaches the model as a summary notice.
                Some(if ids.contains_key(&event.id) {
                    content.clone()
                } else {
                    compaction_summary(&text_of(content))
                })
            })
            .collect();
        let kept: Vec<Content> = compacted[tail_from..]
            .iter()
            .filter_map(|event| event.content().cloned())
            .chain(history[held_from..].iter().cloned())
            .collect();
        shortened.extend(without_thinking(&kept));
        let chars_per_token = self.config.chars_per_token;
        if estimate_content_tokens(&shortened, chars_per_token)
            >= estimate_content_tokens(history, chars_per_token)
        {
            return Outcome::Kept;
        }
        Outcome::Compacted(Box::new(CompactedHistory { history: shortened, event: None }))
    }
}

#[async_trait]
impl HistoryCompactor for IntraInvocationCompactor {
    async fn compact(
        &self,
        ctx: Arc<dyn InvocationContext>,
        model: Arc<dyn Llm>,
        request: HistoryCompactionRequest<'_>,
    ) -> Result<Option<CompactedHistory>> {
        Ok(self.compact_history(ctx, model, request, None).await.compacted())
    }
}

/// What one compaction request came to.
enum Outcome {
    /// The request was below the threshold.
    BelowThreshold,
    /// Every entry is among the most recent ones kept, or after a held call.
    NothingToCompact,
    /// A summary or strategy ran but failed or would not have shrunk the history.
    Kept,
    /// The history was compacted.
    Compacted(Box<CompactedHistory>),
}

impl Outcome {
    fn compacted(self) -> Option<CompactedHistory> {
        match self {
            Self::Compacted(compacted) => Some(*compacted),
            Self::BelowThreshold | Self::NothingToCompact | Self::Kept => None,
        }
    }
}

/// The compactor a runner installs on a run's config: [`IntraInvocationCompactor`] over the
/// run's session, so each compaction is recorded there, with a pause after a compaction that
/// did not help.
pub(crate) struct RunCompactor {
    compactor: Arc<IntraInvocationCompactor>,
    session: Arc<MutableSession>,
    /// Per agent, the history length below which no compaction is tried again, set when one
    /// failed or did not shrink the history, so a run does not pay for a summary every call.
    retry_from: Mutex<HashMap<String, usize>>,
}

impl RunCompactor {
    pub(crate) fn new(
        compactor: Arc<IntraInvocationCompactor>,
        session: Arc<MutableSession>,
    ) -> Self {
        Self { compactor, session, retry_from: Mutex::new(HashMap::new()) }
    }
}

impl std::fmt::Debug for RunCompactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunCompactor").field("compactor", &self.compactor).finish_non_exhaustive()
    }
}

#[async_trait]
impl HistoryCompactor for RunCompactor {
    async fn compact(
        &self,
        ctx: Arc<dyn InvocationContext>,
        model: Arc<dyn Llm>,
        request: HistoryCompactionRequest<'_>,
    ) -> Result<Option<CompactedHistory>> {
        let agent = ctx.agent_name().to_string();
        let retry_from =
            || self.retry_from.lock().unwrap_or_else(PoisonError::into_inner).get(&agent).copied();
        if request.trigger == CompactionTrigger::BeforeModelCall
            && retry_from().is_some_and(|from| request.history.len() < from)
        {
            return Ok(None);
        }
        let history_len = request.history.len();
        // Only an agent reading this run's session — not an agent tool's delegate, which reads
        // its own — has its compaction recorded there.
        let reads_run_session = std::ptr::addr_eq(
            ctx.session() as *const dyn adk_core::Session,
            Arc::as_ptr(&self.session),
        );
        let session = reads_run_session.then_some(self.session.as_ref());
        let outcome = self.compactor.compact_history(ctx, model, request, session).await;
        let mut retry_from = self.retry_from.lock().unwrap_or_else(PoisonError::into_inner);
        match &outcome {
            Outcome::BelowThreshold | Outcome::NothingToCompact => {}
            Outcome::Compacted(_) => {
                retry_from.remove(&agent);
            }
            Outcome::Kept => {
                let step = self.compactor.config.overlap_event_count.max(1);
                retry_from.insert(agent, history_len + step);
            }
        }
        Ok(outcome.compacted())
    }
}

/// `kept` without thinking: a provider binds a turn's thinking to the history it was produced
/// with, and rejects it replayed after a summary.
fn without_thinking(kept: &[Content]) -> impl Iterator<Item = Content> + '_ {
    kept.iter().filter_map(|content| {
        let mut content = content.clone();
        content.parts.retain(|part| !matches!(part, Part::Thinking { .. }));
        (!content.parts.is_empty()).then_some(content)
    })
}

/// `contents` as session-like events for a summarizer: the user's turns authored by the user,
/// the rest by the agent.
fn as_events(ctx: &dyn InvocationContext, contents: &[Content]) -> Vec<Event> {
    contents
        .iter()
        .map(|content| {
            let mut event = Event::new(ctx.invocation_id());
            event.author = match content.role.as_str() {
                "user" => "user".to_string(),
                _ => ctx.agent_name().to_string(),
            };
            event.set_content(content.clone());
            event
        })
        .collect()
}

/// The text of a summarizer's event: its compaction's content, or its own.
fn summary_text(event: &Event) -> String {
    event
        .actions
        .compaction
        .as_ref()
        .map(|compaction| &compaction.compacted_content)
        .or(event.llm_response.content.as_ref())
        .map(text_of)
        .unwrap_or_default()
}

fn text_of(content: &Content) -> String {
    content.parts.iter().filter_map(Part::text).collect::<Vec<_>>().join("\n")
}

/// Where a compaction that summarizes `history[..cut]` is recorded in the session.
struct SessionCut {
    /// The cut, moved earlier to an entry the session can place.
    cut: usize,
    /// The span the compaction covers; its content is the summary, filled in once written.
    span: EventCompaction,
}

/// The latest cut at or before `cut`, in the later half, whose first kept entry is exactly one
/// session event, with the compaction span that hides every event before it and none after.
///
/// The span ends at the last event before that one, a whole millisecond earlier, so it holds
/// in a store that keeps milliseconds only.
fn session_cut(events: &[Event], history: &[Content], cut: usize) -> Option<SessionCut> {
    let earlier = events.iter().rev().find_map(|event| event.actions.compaction.as_ref());
    let visible = |event: &&Event| {
        event.actions.compaction.is_none()
            && !event.llm_response.partial
            && earlier.is_none_or(|compaction| event.timestamp > compaction.end_timestamp)
    };
    (cut.div_ceil(2).max(1)..=cut).rev().filter(|&at| at < history.len()).find_map(|at| {
        if balanced_content_cut(history, at) != at {
            return None;
        }
        let first_kept = &history[at];
        let mut matches = events
            .iter()
            .enumerate()
            .filter(|(_, event)| visible(event) && same_turn(event, first_kept));
        let (index, anchor) = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        let end = events[..index]
            .iter()
            .map(|event| event.timestamp)
            .filter(|timestamp| *timestamp < anchor.timestamp)
            .max()?;
        if events[index..]
            .iter()
            .any(|event| event.timestamp.timestamp_millis() <= end.timestamp_millis())
        {
            return None;
        }
        let start = events
            .first()
            .map(|event| event.timestamp)
            .into_iter()
            .chain(earlier.map(|compaction| compaction.start_timestamp))
            .min()?;
        let span = EventCompaction {
            start_timestamp: start,
            end_timestamp: end,
            compacted_content: Content::new("model"),
        };
        Some(SessionCut { cut: at, span })
    })
}

/// Whether `event` recorded `content`: the same role as history gives it, and the same text,
/// calls and responses.
fn same_turn(event: &Event, content: &Content) -> bool {
    let Some(recorded) = &event.llm_response.content else {
        return false;
    };
    let role = match (event.author.as_str(), recorded.role.as_str()) {
        ("user", _) => "user",
        (_, role @ ("function" | "tool")) => role,
        _ => "model",
    };
    let key = |content: &Content| -> Vec<String> {
        content
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::Text { text } => Some(format!("text:{text}")),
                Part::FunctionCall { name, id, .. } => {
                    Some(format!("call:{}", id.as_deref().unwrap_or(name)))
                }
                Part::FunctionResponse { function_response, id, .. } => {
                    Some(format!("response:{}", id.as_deref().unwrap_or(&function_response.name)))
                }
                _ => None,
            })
            .collect()
    };
    let wanted = key(content);
    role == content.role && !wanted.is_empty() && key(recorded) == wanted
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{
        Content, EventActions, EventCompaction, FunctionResponseData, Part,
        ToolConfirmationRequest, confirmation_pending_response, held_tool_calls,
    };
    use async_trait::async_trait;
    use serde_json::json;

    struct Summary;

    #[async_trait]
    impl BaseEventsSummarizer for Summary {
        async fn summarize_events(&self, events: &[Event]) -> Result<Option<Event>> {
            let mut event = Event::new("compaction");
            event.actions = EventActions {
                compaction: Some(EventCompaction {
                    start_timestamp: events[0].timestamp,
                    end_timestamp: events[events.len() - 1].timestamp,
                    compacted_content: Content::new("model").with_text("summary"),
                }),
                ..Default::default()
            };
            Ok(Some(event))
        }
    }

    fn event(author: &str, parts: Vec<Part>) -> Event {
        let mut event = Event::new("inv-1");
        event.author = author.to_string();
        event.set_content(Content { role: "model".to_string(), parts });
        event
    }

    fn config(
        token_threshold: u64,
        overlap_event_count: usize,
        chars_per_token: u32,
    ) -> IntraCompactionConfig {
        IntraCompactionConfig {
            token_threshold: Some(token_threshold),
            overlap_event_count,
            chars_per_token,
            ..IntraCompactionConfig::default()
        }
    }

    #[tokio::test]
    async fn a_held_call_and_the_events_after_it_are_never_summarized() {
        let mut events: Vec<Event> = (0..6)
            .map(|index| {
                event("user", vec![Part::Text { text: format!("message {index} ").repeat(50) }])
            })
            .collect();
        events.push(event(
            "ops",
            vec![Part::FunctionCall {
                name: "transfer".to_string(),
                args: json!({ "amount": 10 }),
                id: Some("c1".to_string()),
                thought_signature: None,
            }],
        ));
        events.push(event(
            "ops",
            vec![Part::FunctionResponse {
                function_response: FunctionResponseData::new(
                    "transfer",
                    confirmation_pending_response("held"),
                ),
                id: Some("c1".to_string()),
                annotations: None,
            }],
        ));
        let mut hold = Event::new("inv-1");
        hold.author = "ops".to_string();
        hold.actions.tool_confirmation = Some(ToolConfirmationRequest {
            tool_name: "transfer".to_string(),
            function_call_id: Some("c1".to_string()),
            args: json!({ "amount": 10 }),
            acting: None,
            origin: None,
        });
        events.push(hold);
        let start = chrono::Utc::now();
        for (index, event) in events.iter_mut().enumerate() {
            event.timestamp = start + chrono::Duration::milliseconds(index as i64);
        }

        let compactor = IntraInvocationCompactor::new(config(1, 0, 4), Arc::new(Summary));
        let compacted = compactor.maybe_compact(&events).await.unwrap().expect("compacted");
        assert_eq!(compacted.len(), 4, "the summary, then the held call's three events");
        assert_eq!(held_tool_calls(&compacted, "ops", "").len(), 1);
    }

    fn call_and_result(id: &str) -> [Event; 2] {
        [
            event(
                "desk",
                vec![Part::FunctionCall {
                    name: "lookup".to_string(),
                    args: json!({}),
                    id: Some(id.to_string()),
                    thought_signature: None,
                }],
            ),
            event(
                "desk",
                vec![Part::FunctionResponse {
                    function_response: FunctionResponseData::new("lookup", json!({"ok": true})),
                    id: Some(id.to_string()),
                    annotations: None,
                }],
            ),
        ]
    }

    #[tokio::test]
    async fn the_kept_events_never_start_on_a_result_whose_call_was_summarized() {
        let mut events = vec![event("user", vec![Part::Text { text: "background ".repeat(100) }])];
        events.extend(call_and_result("c1"));
        events.push(event("desk", vec![Part::Text { text: "a1".to_string() }]));
        events.push(event("user", vec![Part::Text { text: "q2".to_string() }]));

        // Keeping three events would cut between the call and its result.
        let compactor = IntraInvocationCompactor::new(config(1, 3, 4), Arc::new(Summary));
        let compacted = compactor.maybe_compact(&events).await.unwrap().expect("compacted");
        assert_eq!(compacted.len(), 5, "the summary, then the call, its result, a1 and q2");
        assert_eq!(compacted[1].id, events[1].id);
    }

    #[tokio::test]
    async fn a_summary_that_does_not_shrink_the_history_is_not_used() {
        let mut events = vec![event("user", vec![Part::Text { text: "q".to_string() }])];
        events.push(event("desk", vec![Part::Text { text: "a".to_string() }]));
        events.push(event("user", vec![Part::Text { text: "q2".to_string() }]));
        let compactor = IntraInvocationCompactor::new(config(0, 1, 1), Arc::new(Summary));
        // "summary" is longer than the two one-character events it would replace.
        assert!(compactor.maybe_compact(&events).await.unwrap().is_none());
    }

    fn recorded(author: &str, content: Content, at: chrono::DateTime<chrono::Utc>) -> Event {
        let mut event = Event::new("inv-1");
        event.author = author.to_string();
        event.timestamp = at;
        event.set_content(content);
        event
    }

    fn call(id: &str) -> Content {
        Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "lookup".to_string(),
                args: json!({}),
                id: Some(id.to_string()),
                thought_signature: None,
            }],
        }
    }

    fn result(id: &str) -> Content {
        Content {
            role: "function".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData::new("lookup", json!({"ok": true})),
                id: Some(id.to_string()),
                annotations: None,
            }],
        }
    }

    #[test]
    fn a_compaction_is_recorded_up_to_the_event_before_the_first_kept_entry() {
        let history = vec![
            Content::new("user").with_text("find A1"),
            call("c1"),
            result("c1"),
            call("c2"),
            result("c2"),
        ];
        let start = chrono::Utc::now();
        let ms = |n: i64| start + chrono::Duration::milliseconds(n);
        let events = vec![
            recorded("user", history[0].clone(), ms(0)),
            recorded("desk", history[1].clone(), ms(10)),
            recorded("desk", history[2].clone(), ms(20)),
            recorded("desk", history[3].clone(), ms(30)),
            recorded("desk", history[4].clone(), ms(40)),
        ];
        let cut = session_cut(&events, &history, 3).expect("placed");
        assert_eq!((cut.cut, cut.span.start_timestamp, cut.span.end_timestamp), (3, ms(0), ms(20)));

        // The same text twice cannot be placed, so the cut moves to the call before it.
        let mut repeated = history.clone();
        repeated[4] = Content::new("user").with_text("find A1");
        let mut events = events;
        events[4] = recorded("user", repeated[4].clone(), ms(40));
        let cut = session_cut(&events, &repeated, 4).expect("placed earlier");
        assert_eq!((cut.cut, cut.span.end_timestamp), (3, ms(20)));

        // Events in the same millisecond cannot be told apart by every store.
        events[3].timestamp = ms(20) + chrono::Duration::microseconds(5);
        assert!(session_cut(&events, &repeated, 4).is_none());
    }
}
