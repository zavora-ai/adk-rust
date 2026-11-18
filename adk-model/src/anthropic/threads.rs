//! Message threads (beta `message-threads-2026-08-12`): the API keeps a conversation for a
//! short time, so a later request sends only the messages since the previous response.
//!
//! The session stays the source of truth. A request continues its conversation's thread only
//! when the conversation it sends starts with exactly what the thread holds — every message
//! the thread was created or continued with, then the reply as this client renders it from the
//! agent's history — and its fixed fields are the same. Otherwise it creates a new thread with
//! the whole conversation, so a guardrail or callback that rewrote a turn, a compaction, or a
//! tool change reaches the model as the session records it.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use adk_anthropic::{MessageCreateParams, MessageParam, ThreadParam};

/// The API keeps a thread for five minutes after its last request; past that a continue
/// request would fail, so a new thread is created instead.
const THREAD_TTL: Duration = Duration::from_secs(5 * 60);

/// The most conversations threaded at once; the least recently used is forgotten first.
const MAX_THREADS: usize = 1_024;

/// The thread each recent conversation can continue.
#[derive(Default)]
pub(crate) struct Threads {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    threads: HashMap<u64, Thread>,
    /// Conversations the API would not thread; they are sent without one.
    unthreaded: HashSet<u64>,
}

/// What a thread holds.
struct Thread {
    /// The response the thread continues from.
    last_id: String,
    /// The number of messages it holds, the reply included.
    held: usize,
    /// A hash of those messages, as this client renders them.
    prefix: u64,
    /// A hash of the fields that must stay the same on every request of a thread.
    fixed: u64,
    used: Instant,
}

/// How one request goes out on its conversation's thread.
#[derive(Debug, Clone)]
pub(crate) struct ThreadPlan {
    key: u64,
    fixed: u64,
    /// The request as it would go out without a thread.
    full: MessageCreateParams,
    /// Whether the request continues a thread, rather than creating one.
    pub(crate) continues: bool,
}

impl ThreadPlan {
    /// The request as it would go out without a thread.
    pub(crate) fn unthreaded(&self) -> MessageCreateParams {
        self.full.clone()
    }
}

impl Threads {
    /// Puts `params` on its conversation's thread: a continue request with only the new
    /// messages when the thread holds the rest unchanged, a create request otherwise.
    ///
    /// `None` leaves the request without a thread: a compaction request or a conversation that
    /// carries a compaction block (threads cannot hold one), or a conversation the API would
    /// not thread.
    pub(crate) fn prepare(&self, params: &mut MessageCreateParams) -> Option<ThreadPlan> {
        let key = super::tool_changes::conversation_key(&params.model, params)?;
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if params.requires_compaction_beta() {
            inner.threads.remove(&key);
            return None;
        }
        if inner.unthreaded.contains(&key) {
            return None;
        }
        let full = params.clone();
        let fixed = fixed_fields(params);
        let now = Instant::now();
        inner.threads.retain(|_, thread| now.duration_since(thread.used) < THREAD_TTL);
        // Each response continues once: the thread is recorded again from this request's reply.
        let continues = match inner.threads.remove(&key) {
            Some(thread)
                if thread.fixed == fixed
                    && params.messages.len() > thread.held
                    && prefix(&params.messages[..thread.held]) == thread.prefix =>
            {
                // The beta set is a fixed field, so a continue request sends the betas of the
                // whole conversation, not only of its new messages.
                let mut betas: Vec<String> =
                    params.required_betas().into_iter().map(str::to_string).collect();
                betas.push(adk_anthropic::MESSAGE_THREADS_BETA.to_string());
                params.betas.extend(betas);
                params.messages.drain(..thread.held);
                params.system = None;
                params.tools = None;
                params.thread = Some(ThreadParam::continue_from(thread.last_id));
                true
            }
            _ => {
                params.thread = Some(ThreadParam::Create);
                false
            }
        };
        Some(ThreadPlan { key, fixed, full, continues })
    }

    /// Records that the response `id` answered `plan`'s request with `reply`, as this client
    /// renders it from the agent's history.
    pub(crate) fn record(&self, plan: &ThreadPlan, id: &str, reply: MessageParam) {
        let mut messages = plan.full.messages.clone();
        messages.push(reply);
        let thread = Thread {
            last_id: id.to_string(),
            held: messages.len(),
            prefix: prefix(&messages),
            fixed: plan.fixed,
            used: Instant::now(),
        };
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if inner.threads.len() >= MAX_THREADS
            && let Some(oldest) =
                inner.threads.iter().min_by_key(|(_, thread)| thread.used).map(|(key, _)| *key)
        {
            inner.threads.remove(&oldest);
        }
        inner.threads.insert(plan.key, thread);
    }

    /// Sends `plan`'s conversation without a thread from now on: the API refused to create one.
    pub(crate) fn unthread(&self, plan: &ThreadPlan) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.threads.remove(&plan.key);
        if inner.unthreaded.len() >= MAX_THREADS {
            inner.unthreaded.clear();
        }
        inner.unthreaded.insert(plan.key);
    }
}

/// A hash of the fields a thread fixes: a change to any of them needs a new thread.
fn fixed_fields(params: &MessageCreateParams) -> u64 {
    let fixed = serde_json::json!({
        "model": params.model,
        "tool_choice": params.tool_choice,
        "thinking": params.thinking,
        "context_management": params.context_management,
        "output_config": params.output_config,
        "cache_control": params.cache_control,
        "container": params.container,
        "skills": params.skills,
        "betas": params.required_betas(),
        "extra_betas": params.betas,
        "system": params.system,
        "tools": params.tools,
    });
    let mut hasher = DefaultHasher::new();
    fixed.to_string().hash(&mut hasher);
    hasher.finish()
}

fn prefix(messages: &[MessageParam]) -> u64 {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(messages).unwrap_or_default().hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests;
