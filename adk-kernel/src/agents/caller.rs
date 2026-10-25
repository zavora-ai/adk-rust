//! Who asks the agent table for a change, and from where (spec 3.3 §5.7, §7.2, S-I18).

use adk_core::principal::{SubjectKind, SubjectRef};
use serde::{Deserialize, Serialize};

use crate::control::ControlTransport;

/// How sure the kernel is that the caller is the person at a screen (spec 3.3 §7.2).
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::Presence;
///
/// assert!(!Presence::None.is_present());
/// assert!(Presence::PairedLocal.is_present() && Presence::Confirmed.is_present());
/// assert_eq!(serde_json::to_value(Presence::PairedLocal).unwrap(), "pairedLocal");
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Presence {
    /// Nothing shows the person is there.
    #[default]
    None,
    /// The kernel's own terminal, over its socket or pipe (decision 22).
    PairedLocal,
    /// The person confirmed it is them ("Confirm it's you").
    Confirmed,
}

impl Presence {
    /// Whether this is enough for a high-risk change.
    pub fn is_present(self) -> bool {
        !matches!(self, Self::None)
    }

    /// The presence's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::PairedLocal => "pairedLocal",
            Self::Confirmed => "confirmed",
        }
    }
}

/// The caller of the agent table (spec 3.3 §5.7, S-I18): who, from which surface and how,
/// through which device and transport, with what presence, and the calling process where the
/// operating system reports it. Every `kernel.agent.*` record carries these.
///
/// The table refuses a caller that is not a `Human`, refuses a device, and refuses high
/// risk or money without presence (S-I11).
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::agents::{AgentCaller, Presence};
/// use adk_kernel::control::ControlTransport;
///
/// let caller = AgentCaller::new(SubjectRef::human("alice"), "terminal", "keyboard")
///     .with_presence(Presence::PairedLocal)
///     .with_transport(ControlTransport::Socket)
///     .with_peer_pid(4242);
/// assert!(caller.refusal().is_none());
/// let agent = AgentCaller::new(SubjectRef::service("scheduler"), "web", "pointer");
/// assert!(agent.refusal().is_some());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentCaller {
    /// Who.
    pub by: SubjectRef,
    /// The surface: `web`, `terminal`, `phone`, …
    pub surface: String,
    /// How: `keyboard`, `pointer`, `touch`, …
    pub how: String,
    /// The paired device the request came through, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// How sure the kernel is the person is there.
    #[serde(default)]
    pub presence: Presence,
    /// The control transport the request came on, when it came through the control plane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<ControlTransport>,
    /// The calling process, where the operating system reports it (decision 22).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_pid: Option<u32>,
}

impl AgentCaller {
    /// `by` on `surface`, `how`, without a device, presence, transport or process.
    pub fn new(by: SubjectRef, surface: impl Into<String>, how: impl Into<String>) -> Self {
        Self {
            by,
            surface: surface.into(),
            how: how.into(),
            device: None,
            presence: Presence::None,
            transport: None,
            peer_pid: None,
        }
    }

    /// Sets the device.
    #[must_use]
    pub fn with_device(mut self, device: impl Into<String>) -> Self {
        self.device = Some(device.into());
        self
    }

    /// Sets the presence.
    #[must_use]
    pub fn with_presence(mut self, presence: Presence) -> Self {
        self.presence = presence;
        self
    }

    /// Sets the transport.
    #[must_use]
    pub fn with_transport(mut self, transport: ControlTransport) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Sets the calling process.
    #[must_use]
    pub fn with_peer_pid(mut self, pid: u32) -> Self {
        self.peer_pid = Some(pid);
        self
    }

    /// Why the table refuses this caller any change, whatever its risk: it is not a person
    /// (S-I11), or it came through a paired device, which only answers suggestions (decision
    /// 16). `None` when it may change agents.
    pub fn refusal(&self) -> Option<String> {
        if self.by.kind != SubjectKind::Human {
            return Some(format!(
                "only a person adds, changes, pauses or removes an agent, and {} is not one \
                 (spec 3.3 S-I11)",
                self.by.id
            ));
        }
        if self.device.is_some() {
            return Some(
                "a paired device answers suggested agents only; set up and change agents in \
                 the web home or the terminal (spec 3.3 decision 16)"
                    .to_string(),
            );
        }
        None
    }
}
