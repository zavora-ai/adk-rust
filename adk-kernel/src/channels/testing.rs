//! The in-memory test channel (spec 3.3 §10 K1).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{ChannelAnswer, ChannelInbound, ChannelNotice, ChannelSink, DecisionCard, StopNotice};
use crate::error::Result;

/// One thing a channel sent to a thread (spec 3.3 §10 K1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
#[non_exhaustive]
pub enum Delivered {
    /// An agent's reply or other text.
    Message {
        /// The text.
        text: String,
    },
    /// A decision card.
    Decision {
        /// The card.
        card: DecisionCard,
    },
    /// The notice that everything was stopped.
    Stopped {
        /// The notice.
        notice: StopNotice,
    },
    /// A notice.
    Notice {
        /// The notice.
        notice: ChannelNotice,
    },
}

/// A chat channel in process memory (spec 3.3 §10 K1): a [`ChannelSink`] that keeps what it
/// is sent per thread, and makes the [`ChannelInbound`] values a platform adapter would, for
/// tests and the contract suite.
///
/// It offers a card's [`answers`](DecisionCard::answers) and nothing else, as every adapter
/// must: [`press`](Self::press) of an answer the card does not offer is `None`.
///
/// # Example
///
/// ```rust
/// use adk_kernel::channels::{ChannelSink, InMemoryChannel};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let chat = InMemoryChannel::new("chat");
/// chat.send_message("t1", "hello").await?;
/// assert_eq!(chat.messages("t1"), ["hello"]);
/// let inbound = chat.text("ana", "t1", "stop all");
/// assert_eq!(inbound.channel, "chat");
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct InMemoryChannel {
    name: String,
    threads: Mutex<BTreeMap<String, Vec<Delivered>>>,
    next_message: AtomicU64,
}

impl InMemoryChannel {
    /// A channel named `name` with nothing sent.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), threads: Mutex::default(), next_message: AtomicU64::new(1) }
    }

    fn push(&self, thread: &str, delivered: Delivered) {
        self.threads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(thread.to_string())
            .or_default()
            .push(delivered);
    }

    /// Everything sent to `thread`, in order.
    pub fn delivered(&self, thread: &str) -> Vec<Delivered> {
        self.threads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(thread)
            .cloned()
            .unwrap_or_default()
    }

    /// Everything sent to `thread` as the JSON that left the machine, one value per line.
    pub fn wire(&self, thread: &str) -> String {
        self.delivered(thread)
            .iter()
            .map(|delivered| serde_json::to_string(delivered).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The texts sent to `thread`.
    pub fn messages(&self, thread: &str) -> Vec<String> {
        self.delivered(thread)
            .into_iter()
            .filter_map(|delivered| match delivered {
                Delivered::Message { text } => Some(text),
                _ => None,
            })
            .collect()
    }

    /// The notices sent to `thread`.
    pub fn notices(&self, thread: &str) -> Vec<ChannelNotice> {
        self.delivered(thread)
            .into_iter()
            .filter_map(|delivered| match delivered {
                Delivered::Notice { notice } => Some(notice),
                _ => None,
            })
            .collect()
    }

    /// The last decision card sent to `thread`.
    pub fn last_card(&self, thread: &str) -> Option<DecisionCard> {
        self.delivered(thread).into_iter().rev().find_map(|delivered| match delivered {
            Delivered::Decision { card } => Some(card),
            _ => None,
        })
    }

    /// The last pairing code sent to `thread`.
    pub fn pairing_code(&self, thread: &str) -> Option<String> {
        self.notices(thread).into_iter().rev().find_map(|notice| match notice {
            ChannelNotice::PairingCode { code, .. } => Some(code),
            _ => None,
        })
    }

    /// Forgets what was sent to `thread`.
    pub fn clear(&self, thread: &str) {
        self.threads.lock().unwrap_or_else(PoisonError::into_inner).remove(thread);
    }

    fn message_id(&self) -> String {
        format!("m{}", self.next_message.fetch_add(1, Ordering::SeqCst))
    }

    /// `account` typing `text` in `thread`, as a new message.
    pub fn text(&self, account: &str, thread: &str, text: &str) -> ChannelInbound {
        ChannelInbound::text(&self.name, account, thread, self.message_id(), text)
    }

    /// `account` pressing `answer` on `card` in `thread`, or `None` when the card does not
    /// offer it.
    pub fn press(
        &self,
        account: &str,
        thread: &str,
        card: &DecisionCard,
        answer: ChannelAnswer,
    ) -> Option<ChannelInbound> {
        card.answers.contains(&answer).then(|| {
            ChannelInbound::answer(&self.name, account, thread, self.message_id(), card, answer)
        })
    }
}

#[async_trait]
impl ChannelSink for InMemoryChannel {
    fn channel(&self) -> &str {
        &self.name
    }

    async fn send_message(&self, thread: &str, text: &str) -> Result<()> {
        self.push(thread, Delivered::Message { text: text.to_string() });
        Ok(())
    }

    async fn send_decision(&self, thread: &str, card: &DecisionCard) -> Result<()> {
        self.push(thread, Delivered::Decision { card: card.clone() });
        Ok(())
    }

    async fn send_stopped(&self, thread: &str, notice: &StopNotice) -> Result<()> {
        self.push(thread, Delivered::Stopped { notice: notice.clone() });
        Ok(())
    }

    async fn send_notice(&self, thread: &str, notice: &ChannelNotice) -> Result<()> {
        self.push(thread, Delivered::Notice { notice: notice.clone() });
        Ok(())
    }
}
