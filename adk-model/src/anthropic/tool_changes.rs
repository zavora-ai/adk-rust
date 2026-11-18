//! Cache-safe tool changes (beta `inline-tools-2026-09-15`).
//!
//! The `tools` array opens the prompt prefix, so a request whose tools differ from the
//! previous request's misses the whole prompt cache, and on models with preserved thinking
//! invalidates every earlier thinking block. With inline tool changes, a conversation keeps
//! sending the `tools` array it started with, and a tool added, redefined or removed later
//! reaches Claude as a `tool_addition` or `tool_removal` block in a `role: "system"` message
//! after the user turn where the change happened. Only that message and what follows it are
//! new input.
//!
//! The client remembers, per conversation, the tools it started with and each change it sent
//! ([`ToolLedgers`]). A conversation is told apart by its model, system prompt and first block,
//! and each change by the exact messages before it: a request whose history no longer starts
//! with them starts over with its tools as they are, which is what it would have sent without
//! inline changes.

use std::collections::{BTreeMap, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use adk_anthropic::{
    CompactionBlock, ContentBlock, MessageCreateParams, MessageParam, MessageParamContent,
    MessageRole, Model, ToolAdditionBlock, ToolChangeTarget, ToolRemovalBlock, ToolUnionParam,
};
use serde_json::Value;

/// A conversation not seen for this long is forgotten; its next request starts over.
const LEDGER_TTL: Duration = Duration::from_secs(60 * 60);

/// The most conversations remembered at once; the least recently used is forgotten first.
const MAX_LEDGERS: usize = 1_024;

/// The tools each recent conversation started with and the changes it sent since.
#[derive(Default)]
pub(crate) struct ToolLedgers {
    ledgers: Mutex<HashMap<u64, Ledger>>,
}

/// What one conversation's requests declared.
#[derive(Debug, Clone)]
struct Ledger {
    /// The `tools` array the conversation started with, sent on every request.
    baseline: Vec<ToolUnionParam>,
    /// The tools offered before any change: the baseline, or for a conversation that continues
    /// a compaction, the baseline with the changes the summary records.
    start: BTreeMap<String, Value>,
    /// The changes sent since, in order.
    changes: Vec<Change>,
    /// The thinking blocks in the first `n` messages are left out: they were produced with a
    /// tool change the conversation no longer shows.
    strip_thinking_before: usize,
    /// Set on a conversation that continues a compaction whose later tool changes are gone, so
    /// its first request sets `strip_thinking_before`.
    strip_kept_thinking: bool,
    used: Instant,
}

/// One tool change: the tools added or redefined and those removed, sent after the first
/// `after` messages, which hash to `prefix`.
#[derive(Debug, Clone)]
struct Change {
    after: usize,
    prefix: u64,
    added: Vec<Value>,
    removed: Vec<String>,
}

/// The tools a compaction request offered, so the conversation that continues from its
/// summary keeps sending the same `tools` array.
#[derive(Debug, Clone)]
pub(crate) struct CompactionTools {
    baseline: Vec<ToolUnionParam>,
    /// The tools offered after the compacted messages.
    offered: BTreeMap<String, Value>,
    /// Whether the conversation had tool changes after the compacted messages, which the
    /// summary does not record.
    changed_after: bool,
}

impl Ledger {
    fn new(baseline: Vec<ToolUnionParam>) -> Self {
        Self {
            start: by_name(&baseline),
            baseline,
            changes: Vec::new(),
            strip_thinking_before: 0,
            strip_kept_thinking: false,
            used: Instant::now(),
        }
    }

    /// The tools offered after the first `messages` messages: the baseline with every change
    /// sent before then applied.
    fn offered_at(&self, messages: usize) -> BTreeMap<String, Value> {
        let mut offered = self.start.clone();
        for change in self.changes.iter().filter(|change| change.after <= messages) {
            for name in &change.removed {
                offered.remove(name);
            }
            for definition in &change.added {
                if let Some(name) = tool_name(definition) {
                    offered.insert(name, definition.clone());
                }
            }
        }
        offered
    }

    /// Whether `messages` still starts with what each change sent within the first `len`
    /// messages followed.
    fn holds(&self, messages: &[MessageParam], len: usize) -> bool {
        self.changes.iter().filter(|change| change.after <= len).all(|change| {
            change.after <= messages.len() && prefix(&messages[..change.after]) == change.prefix
        })
    }
}

impl ToolLedgers {
    /// Sends `params` as its conversation's next request: the `tools` array the conversation
    /// started with, and every tool change sent since in place, with a new change for any
    /// tool the request adds, redefines or removes.
    ///
    /// A compaction request records no change; it carries the changes sent before the
    /// messages it summarizes, so the summary records them.
    pub(crate) fn apply(&self, params: &mut MessageCreateParams) {
        let Some(key) = conversation_key(&params.model, params) else {
            return;
        };
        let tools = params.tools.clone().unwrap_or_default();
        let len = params.messages.len();
        let compaction = params.compaction.is_some();
        let mut ledgers = self.ledgers.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        ledgers.retain(|_, ledger| now.duration_since(ledger.used) < LEDGER_TTL);
        let held = ledgers.get(&key).is_some_and(|ledger| {
            ledger.holds(&params.messages, if compaction { len } else { usize::MAX })
        });
        if !held {
            if !compaction {
                if ledgers.len() >= MAX_LEDGERS
                    && let Some(oldest) =
                        ledgers.iter().min_by_key(|(_, ledger)| ledger.used).map(|(key, _)| *key)
                {
                    ledgers.remove(&oldest);
                }
                ledgers.insert(key, Ledger::new(tools));
            }
            return;
        }
        let Some(ledger) = ledgers.get_mut(&key) else { return };
        ledger.used = now;
        if ledger.strip_kept_thinking {
            ledger.strip_kept_thinking = false;
            ledger.strip_thinking_before = len;
        }
        if !compaction {
            record_change(ledger, &params.messages, &tools);
        }
        render(ledger, params, len);
    }

    /// The tools `params`, a compaction request, offers, for [`continue_after`](Self::continue_after).
    pub(crate) fn compaction_tools(&self, params: &MessageCreateParams) -> Option<CompactionTools> {
        let key = conversation_key(&params.model, params)?;
        let ledgers = self.ledgers.lock().unwrap_or_else(PoisonError::into_inner);
        let ledger = ledgers.get(&key)?;
        let len = params.messages.len();
        Some(CompactionTools {
            baseline: ledger.baseline.clone(),
            offered: ledger.offered_at(len),
            changed_after: ledger.changes.iter().any(|change| change.after > len),
        })
    }

    /// Keeps the tools of the conversation a compaction summarized for the conversation that
    /// opens with `block`: the API applies the tool changes the block records on top of the
    /// same `tools` array.
    pub(crate) fn continue_after(
        &self,
        model: &Model,
        params: &MessageCreateParams,
        block: &CompactionBlock,
        tools: CompactionTools,
    ) {
        let Some(key) = key_of(model, params, &ContentBlock::Compaction(block.clone())) else {
            return;
        };
        let mut ledger = Ledger::new(tools.baseline);
        ledger.start = tools.offered;
        ledger.strip_kept_thinking = tools.changed_after;
        self.ledgers.lock().unwrap_or_else(PoisonError::into_inner).insert(key, ledger);
    }
}

/// Records the tools `tools` adds, redefines or removes against what the conversation offers
/// after `messages`, as a change after its last user turn. A change can follow only a user
/// turn; where it cannot, the conversation starts over with its tools as they are.
fn record_change(ledger: &mut Ledger, messages: &[MessageParam], tools: &[ToolUnionParam]) {
    let len = messages.len();
    // A change already sent at this position is replaced by one that reaches the same tools.
    let previous = ledger.changes.last().filter(|change| change.after == len).cloned();
    if previous.is_some() {
        ledger.changes.pop();
    }
    let offered = ledger.offered_at(len);
    let current = by_name(tools);
    let added: Vec<Value> = current
        .iter()
        .filter(|(name, definition)| offered.get(*name) != Some(definition))
        .map(|(_, definition)| definition.clone())
        .collect();
    let removed: Vec<String> =
        offered.keys().filter(|name| !current.contains_key(*name)).cloned().collect();
    if added.is_empty() && removed.is_empty() {
        return;
    }
    if messages.last().is_none_or(|message| message.role != MessageRole::User) {
        tracing::debug!("a tool change can follow only a user turn; the conversation starts over");
        *ledger = Ledger::new(tools.to_vec());
        return;
    }
    tracing::debug!(
        tools.added = added.len(),
        tools.removed = removed.len(),
        "sending a tool change in place of a new tools array"
    );
    ledger.changes.push(Change { after: len, prefix: prefix(messages), added, removed });
}

/// Puts the conversation's `tools` array and its tool changes into `params`, whose first `len`
/// messages are the conversation's.
fn render(ledger: &Ledger, params: &mut MessageCreateParams, len: usize) {
    params.tools = (!ledger.baseline.is_empty()).then(|| ledger.baseline.clone());
    for message in params.messages.iter_mut().take(ledger.strip_thinking_before) {
        if let MessageParamContent::Array(blocks) = &mut message.content
            && blocks.iter().any(|block| !is_thinking(block))
        {
            blocks.retain(|block| !is_thinking(block));
        }
    }
    // Inserted from the last, so each position still counts the conversation's messages only.
    for change in ledger.changes.iter().rev().filter(|change| change.after <= len) {
        let mut blocks: Vec<ContentBlock> = change
            .added
            .iter()
            .map(|definition| {
                ContentBlock::ToolAddition(ToolAdditionBlock {
                    tool: ToolChangeTarget::definition(definition.clone()),
                })
            })
            .collect();
        blocks.extend(change.removed.iter().map(|name| {
            ContentBlock::ToolRemoval(ToolRemovalBlock { tool: ToolChangeTarget::reference(name) })
        }));
        params
            .messages
            .insert(change.after, MessageParam::new_with_blocks(blocks, MessageRole::System));
    }
}

fn is_thinking(block: &ContentBlock) -> bool {
    matches!(block, ContentBlock::Thinking(_) | ContentBlock::RedactedThinking(_))
}

fn tool_name(definition: &Value) -> Option<String> {
    definition.get("name")?.as_str().map(str::to_string)
}

fn by_name(tools: &[ToolUnionParam]) -> BTreeMap<String, Value> {
    tools
        .iter()
        .filter_map(|tool| serde_json::to_value(tool).ok())
        .filter_map(|definition| Some((tool_name(&definition)?, definition)))
        .collect()
}

/// A hash of `messages` as they are sent.
fn prefix(messages: &[MessageParam]) -> u64 {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(messages).unwrap_or_default().hash(&mut hasher);
    hasher.finish()
}

/// What tells a conversation apart: its model, system prompt and first block.
pub(super) fn conversation_key(model: &Model, params: &MessageCreateParams) -> Option<u64> {
    let first = match &params.messages.first()?.content {
        MessageParamContent::Array(blocks) => blocks.first()?.clone(),
        MessageParamContent::String(text) => {
            ContentBlock::Text(adk_anthropic::TextBlock::new(text.clone()))
        }
    };
    key_of(model, params, &first)
}

fn key_of(model: &Model, params: &MessageCreateParams, first: &ContentBlock) -> Option<u64> {
    let mut first = serde_json::to_value(first).ok()?;
    // A breakpoint placed on the first block does not make it another conversation.
    if let Some(block) = first.as_object_mut() {
        block.remove("cache_control");
    }
    let mut hasher = DefaultHasher::new();
    model.to_string().hash(&mut hasher);
    serde_json::to_string(&params.system).unwrap_or_default().hash(&mut hasher);
    first.to_string().hash(&mut hasher);
    Some(hasher.finish())
}

#[cfg(test)]
mod tests;
