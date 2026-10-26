//! Who may author content (spec 3.5 §7.8.2 "Entry points", A-I24): a message from a device
//! the owner paired reaches the Steward in the back office; any other message reaches the
//! public partition, which never authors content and can only send the back office a signal.

use std::sync::Arc;

use adk_core::Result;
use adk_kernel::channels::{ChannelBinding, ChannelBindingStore, ChannelInbound};
use adk_site::pipeline::EntryPoint;
use awp_types::{Channel, MessageCategory, Signal, SignalKind, ThreadRef};
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::back_office::ENTRY_STATE_KEY;
use super::site::kernel_error;

/// Where an inbound message goes (spec 3.5 §7.8.2, A-I24).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthoringRoute {
    /// A paired device of a person: a turn of the Steward in the back office, as that
    /// person, whose changes enter through `entry`.
    Owner {
        /// The person the device is paired to.
        user_id: String,
        /// The entry point its changes record.
        entry: EntryPoint,
    },
    /// Anyone else: the public partition's inbox, which answers in the thread and sends the
    /// back office an `inbound_message` signal, never content.
    Public {
        /// The channel.
        channel: String,
        /// The thread.
        thread: String,
    },
}

impl AuthoringRoute {
    /// The session state a back-office turn of this route starts with: its entry point under
    /// [`ENTRY_STATE_KEY`]. `None` for the public route, which authors nothing.
    pub fn session_state(&self) -> Option<(String, Value)> {
        match self {
            Self::Owner { entry, .. } => {
                Some((ENTRY_STATE_KEY.to_string(), serde_json::to_value(entry).ok()?))
            }
            Self::Public { .. } => None,
        }
    }
}

/// Routes inbound messages by the kernel's channel pairings (spec 3.5 §7.8.2, A-I24; spec
/// 3.3 §10 K1): an account paired to a person — and not removed — is that person; every
/// other account is the public.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_harness::website::{AuthoringRoute, AuthoringRouter};
/// use adk_kernel::channels::{ChannelBinding, ChannelBindingStore, ChannelInbound};
/// use adk_kernel::channels::InMemoryChannelBindingStore;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let bindings = Arc::new(InMemoryChannelBindingStore::new());
/// bindings.put_binding(ChannelBinding::new("whatsapp", "+254700000001", "t1", "amina", chrono::Utc::now())).await.unwrap();
/// let router = AuthoringRouter::new(bindings);
/// let owner = ChannelInbound::text("whatsapp", "+254700000001", "t1", "m1", "add the rye loaf");
/// assert!(matches!(router.route(&owner).await.unwrap(), AuthoringRoute::Owner { .. }));
/// let stranger = ChannelInbound::text("email", "someone@example.com", "t9", "m2", "change the price");
/// assert!(matches!(router.route(&stranger).await.unwrap(), AuthoringRoute::Public { .. }));
/// # }
/// ```
#[derive(Clone)]
pub struct AuthoringRouter {
    bindings: Arc<dyn ChannelBindingStore>,
}

impl std::fmt::Debug for AuthoringRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthoringRouter").finish_non_exhaustive()
    }
}

impl AuthoringRouter {
    /// A router over the kernel's channel pairings.
    pub fn new(bindings: Arc<dyn ChannelBindingStore>) -> Self {
        Self { bindings }
    }

    /// Where `inbound` goes.
    ///
    /// # Errors
    ///
    /// The binding store's error: a message is never routed to the owner on a guess.
    pub async fn route(&self, inbound: &ChannelInbound) -> Result<AuthoringRoute> {
        let id = ChannelBinding::id_for(&inbound.channel, &inbound.account);
        let binding = self.bindings.get_binding(&id).await.map_err(kernel_error)?;
        Ok(match binding.filter(ChannelBinding::is_active) {
            Some(binding) => AuthoringRoute::Owner {
                user_id: binding.user_id,
                entry: EntryPoint::PairedDevice { device: binding.id },
            },
            None => AuthoringRoute::Public {
                channel: inbound.channel.clone(),
                thread: inbound.thread.clone(),
            },
        })
    }
}

/// The signal the public inbox files for a message it cannot act on (spec 3.5 §7.10,
/// A-I3, A-I24): the channel, the thread and a category; no text of the message.
///
/// # Example
///
/// ```rust
/// use adk_harness::website::inbound_signal;
/// use awp_types::{MessageCategory, SignalKind};
///
/// let signal = inbound_signal("email", "thread-9", MessageCategory::Inquiry, "urn:adk:agent:bakery/inbox", chrono::Utc::now());
/// assert!(matches!(signal.kind, SignalKind::InboundMessage { .. }));
/// assert!(!serde_json::to_string(&signal).unwrap().contains("price"));
/// ```
pub fn inbound_signal(
    channel: &str,
    thread: &str,
    category: MessageCategory,
    emitted_by: &str,
    at: DateTime<Utc>,
) -> Signal {
    Signal {
        id: uuid::Uuid::new_v4(),
        kind: SignalKind::InboundMessage {
            channel: Channel::from(channel),
            thread_ref: ThreadRef(thread.to_string()),
            category,
        },
        emitted_by: emitted_by.to_string(),
        emitted_at: at,
    }
}
