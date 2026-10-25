//! Who acts, from which surface, and how the input was given (spec 3.3 §6, §7.2, §11).

use adk_core::principal::SubjectRef;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::words;

/// The surface a person uses (spec 3.3 §5, §7.2).
///
/// What a surface may decide is fixed by §7.2 and computed by
/// [`rules::allowed_answers`](crate::rules::allowed_answers); a surface never grants more
/// than the web home.
///
/// # Example
///
/// ```rust
/// use adk_surface::SurfaceKind;
///
/// assert_eq!(serde_json::to_value(SurfaceKind::Tray).unwrap(), "tray");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SurfaceKind {
    /// The web home (spec 3.3 §5.1).
    Web,
    /// The terminal (spec 3.3 §5.2).
    Terminal,
    /// The web home installed on a phone (spec 3.3 §5.4).
    Phone,
    /// The desktop tray (spec 3.3 §5.5).
    Tray,
    /// A chat channel (spec 3.3 §5.3).
    Chat,
    /// Voice and video (spec 3.3 §5.6).
    Voice,
}

/// How an input was given (spec 3.3 §6, §11): every command carries one, and it reaches the
/// audit record, so "What your agents did" can say "from Telegram".
///
/// # Example
///
/// ```rust
/// use adk_surface::Origin;
///
/// let origin = Origin::Chat { channel: "Telegram".into() };
/// assert_eq!(serde_json::to_value(&origin).unwrap(), serde_json::json!({ "kind": "chat", "channel": "Telegram" }));
/// assert_eq!(serde_json::to_value(Origin::Touch).unwrap(), serde_json::json!({ "kind": "touch" }));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Origin {
    /// Typed.
    Keyboard,
    /// Clicked.
    Pointer,
    /// Tapped.
    Touch,
    /// Spoken.
    Voice,
    /// Sent from a chat channel.
    Chat {
        /// The channel's name, such as `Telegram`.
        channel: String,
    },
    /// Requested by an agent (spec 3.3 §7.4 rule 6: never an answer to a decision).
    Agent {
        /// The requesting agent.
        agent: String,
    },
}

impl Origin {
    /// The wire name of the kind: `keyboard`, `pointer`, `touch`, `voice`, `chat` or `agent`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Keyboard => "keyboard",
            Self::Pointer => "pointer",
            Self::Touch => "touch",
            Self::Voice => "voice",
            Self::Chat { .. } => "chat",
            Self::Agent { .. } => "agent",
        }
    }
}

/// Where and how an answer was given, as a decision answered elsewhere shows it
/// (spec 3.3 §8.1 "Decided").
///
/// # Example
///
/// ```rust
/// use adk_surface::{Origin, SurfaceKind, Via};
///
/// let via = Via { surface: SurfaceKind::Chat, origin: Origin::Chat { channel: "Telegram".into() } };
/// assert_eq!(via.phrase(), "in Telegram");
/// let via = Via { surface: SurfaceKind::Phone, origin: Origin::Touch };
/// assert_eq!(via.phrase(), "on your phone");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Via {
    /// The surface.
    pub surface: SurfaceKind,
    /// How the answer was given.
    pub origin: Origin,
}

impl Via {
    /// The place in plain words: `"in Telegram"`, `"on your phone"`, `"by voice"`.
    pub fn phrase(&self) -> String {
        match (&self.origin, self.surface) {
            (Origin::Chat { channel }, _) => words::in_channel(channel),
            (Origin::Voice, _) | (_, SurfaceKind::Voice) => words::BY_VOICE.to_string(),
            (_, SurfaceKind::Web) => words::IN_THE_HOME_APP.to_string(),
            (_, SurfaceKind::Phone) => words::ON_YOUR_PHONE.to_string(),
            (_, SurfaceKind::Terminal) => words::IN_THE_TERMINAL.to_string(),
            (_, SurfaceKind::Tray) => words::FROM_THE_TRAY.to_string(),
            (_, SurfaceKind::Chat) => words::in_channel("chat"),
        }
    }
}

/// The authenticated caller of an engine read or command (spec 3.3 §6, §11 S-I8).
///
/// The subject and its groups come from the server's authentication, never from a request
/// body. So do the two facts that widen what a caller may decide —
/// [`presence_confirmed`](Self::presence_confirmed) and [`paired_local`](Self::paired_local) —
/// which the web router reads from the request context the server's extractor built. The
/// surface and origin a client declares can only narrow what it may decide: no surface allows
/// more than the web home.
///
/// # Example
///
/// ```rust
/// use adk_surface::{Caller, Origin, SurfaceKind};
///
/// let caller = Caller::human("ana", SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() });
/// assert_eq!(caller.subject.id, "ana");
/// assert!(!caller.presence_confirmed);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// Who: the session owner a surface checks every read and command against (S-I8).
    pub subject: SubjectRef,
    /// The groups authentication established, for the approval service's eligibility check.
    pub groups: Vec<String>,
    /// The surface.
    pub surface: SurfaceKind,
    /// How the input was given.
    pub origin: Origin,
    /// The person confirmed it is them just now (a passkey or the device's lock), which
    /// high-risk approvals on the web home and the phone need (spec 3.3 §7.2, §8.2).
    pub presence_confirmed: bool,
    /// A terminal on the kernel's own machine, paired, which may approve high-risk calls and
    /// resume after a stop (spec 3.3 §7.2, §11 S-I10).
    pub paired_local: bool,
    /// The paired device the caller acts from, when it authenticated with a device token
    /// (spec 3.3 §10 K2, §11): it reaches every audit record, so "Where you can say yes
    /// from" and the record name the device.
    pub device: Option<String>,
}

impl Caller {
    /// A person, by their user id.
    pub fn human(user_id: impl Into<String>, surface: SurfaceKind, origin: Origin) -> Self {
        Self {
            subject: SubjectRef::human(user_id),
            groups: Vec::new(),
            surface,
            origin,
            presence_confirmed: false,
            paired_local: false,
            device: None,
        }
    }

    /// Sets the groups.
    #[must_use]
    pub fn with_groups(mut self, groups: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.groups = groups.into_iter().map(Into::into).collect();
        self
    }

    /// Marks that the person confirmed it is them just now. Set only from server-side
    /// authentication.
    #[must_use]
    pub fn with_presence_confirmed(mut self) -> Self {
        self.presence_confirmed = true;
        self
    }

    /// Marks a paired local terminal. Set only from the transport (the kernel's socket or
    /// pipe) or a device token scoped to it.
    #[must_use]
    pub fn with_paired_local(mut self) -> Self {
        self.paired_local = true;
        self
    }

    /// Sets the paired device the caller acts from (spec 3.3 §10 K2, §11). Set only from
    /// server-side authentication: a device token's context.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_surface::{Caller, Origin, SurfaceKind};
    ///
    /// let caller = Caller::human("ana", SurfaceKind::Phone, Origin::Touch).with_device("dev_1");
    /// assert_eq!(caller.device.as_deref(), Some("dev_1"));
    /// ```
    #[must_use]
    pub fn with_device(mut self, id: impl Into<String>) -> Self {
        self.device = Some(id.into());
        self
    }

    /// The session owner the caller acts as.
    pub fn user_id(&self) -> &str {
        &self.subject.id
    }

    /// Where and how this caller answers.
    pub fn via(&self) -> Via {
        Via { surface: self.surface, origin: self.origin.clone() }
    }
}
