//! Signals: the typed records public agents send to the back office (spec 3.5 §7.10).

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::identity::RequesterId;

/// A typed record a public-partition agent appends to the back office's agenda through
/// `emit_signal` (spec 3.5 §7.10).
///
/// A signal reads nothing back and carries only typed fields; free text arrives as
/// [`Bounded`] strings marked untrusted (A-I3).
///
/// # Example
///
/// ```
/// use awp_types::{Bounded, Signal, SignalKind};
/// use chrono::Utc;
/// use uuid::Uuid;
///
/// let signal = Signal {
///     id: Uuid::nil(),
///     kind: SignalKind::ContentGap {
///         topic: Bounded::new("installation cost", 120).unwrap(),
///         question_count: 3,
///         samples: vec![Bounded::truncated("How much does a 5 kW system cost to install?", 200)],
///     },
///     emitted_by: "urn:adk:agent:example/concierge".into(),
///     emitted_at: Utc::now(),
/// };
/// assert_eq!(signal.kind.name(), "content_gap");
/// let json = serde_json::to_value(&signal).unwrap();
/// assert_eq!(json["kind"]["type"], "content_gap");
/// assert_eq!(json["kind"]["topic"]["untrusted"], true);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Signal {
    /// Signal id.
    pub id: Uuid,
    /// What happened, as typed fields.
    pub kind: SignalKind,
    /// Principal of the public agent that emitted it.
    pub emitted_by: String,
    /// When it was emitted.
    pub emitted_at: DateTime<Utc>,
}

/// What a signal reports (spec 3.5 §7.10).
///
/// Serialized with a `type` tag: `{"type": "content_gap", "topic": {...}, ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum SignalKind {
    /// A visitor left contact details with consent.
    LeadCaptured {
        /// The lead record.
        lead_ref: LeadRef,
        /// Where the lead came from.
        source: Channel,
        /// The consent receipts the visitor granted.
        consent: Vec<ConsentReceiptRef>,
        /// Answers to qualifying questions, by question id.
        answers: BTreeMap<String, Bounded>,
    },
    /// Visitors asked about something the site does not answer.
    ContentGap {
        /// The topic.
        topic: Bounded,
        /// How many questions asked about it.
        question_count: u32,
        /// Sample questions.
        samples: Vec<Bounded>,
    },
    /// A visitor showed intent to use a capability.
    VisitorIntent {
        /// The capability.
        capability: String,
        /// How it ended.
        outcome: Outcome,
    },
    /// A conversation needs a person.
    SupportEscalation {
        /// Why.
        reason: ReasonCode,
        /// How to reach the person who asked.
        contact_ref: ContactRef,
    },
    /// A review was seen on a platform.
    ReviewSeen {
        /// The platform.
        platform: String,
        /// Rating, one to five.
        rating: u8,
        /// An excerpt.
        excerpt: Bounded,
    },
    /// A message arrived on a public channel.
    InboundMessage {
        /// The channel.
        channel: Channel,
        /// The thread it belongs to.
        thread_ref: ThreadRef,
        /// What kind of message it is.
        category: MessageCategory,
    },
    /// A verified agent called a capability.
    AgentInteraction {
        /// The verified requester.
        requester: RequesterId,
        /// The capability.
        capability: String,
        /// How it ended.
        outcome: Outcome,
    },
    /// A requester behaved abusively.
    AbuseObserved {
        /// The requester.
        requester: RequesterId,
        /// What it did.
        kind: AbuseKind,
    },
}

impl SignalKind {
    /// Returns the snake_case name of the kind, as serialized in `type`.
    pub fn name(&self) -> &'static str {
        match self {
            SignalKind::LeadCaptured { .. } => "lead_captured",
            SignalKind::ContentGap { .. } => "content_gap",
            SignalKind::VisitorIntent { .. } => "visitor_intent",
            SignalKind::SupportEscalation { .. } => "support_escalation",
            SignalKind::ReviewSeen { .. } => "review_seen",
            SignalKind::InboundMessage { .. } => "inbound_message",
            SignalKind::AgentInteraction { .. } => "agent_interaction",
            SignalKind::AbuseObserved { .. } => "abuse_observed",
        }
    }
}

/// A length-limited string from untrusted input, always marked untrusted (spec 3.5 §7.10,
/// A-I3).
///
/// The constructor enforces the limit; deserialization enforces the ceiling of 4,096
/// characters and marks the value untrusted whatever the input says. Screening for injection and redacting
/// personal data happen in the harness before a value is built.
///
/// Serialized as `{"text": "...", "untrusted": true}`.
///
/// # Example
///
/// ```
/// use awp_types::Bounded;
///
/// let topic = Bounded::new("opening hours", 40).unwrap();
/// assert!(topic.is_untrusted());
/// assert!(Bounded::new("x".repeat(41), 40).is_err());
/// assert_eq!(Bounded::truncated("abcdef", 3).as_str(), "abc");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Bounded {
    text: String,
}

impl Bounded {
    /// The longest text any `Bounded` holds, in characters.
    pub const MAX_CHARS: usize = 4_096;

    /// Wraps `text` when it has at most `max` characters.
    ///
    /// # Errors
    ///
    /// Returns [`BoundedError::TooLong`] when `text` exceeds `max` characters and
    /// [`BoundedError::LimitTooLarge`] when `max` exceeds [`Bounded::MAX_CHARS`].
    pub fn new(text: impl Into<String>, max: usize) -> Result<Self, BoundedError> {
        if max > Self::MAX_CHARS {
            return Err(BoundedError::LimitTooLarge { max });
        }
        let text = text.into();
        let chars = text.chars().count();
        if chars > max {
            return Err(BoundedError::TooLong { chars, max });
        }
        Ok(Self { text })
    }

    /// Wraps the first `max` characters of `text`, at most [`Bounded::MAX_CHARS`].
    pub fn truncated(text: &str, max: usize) -> Self {
        let max = max.min(Self::MAX_CHARS);
        Self { text: text.chars().take(max).collect() }
    }

    /// Returns the text.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Returns `true`: a `Bounded` value is always untrusted input.
    pub fn is_untrusted(&self) -> bool {
        true
    }

    /// Returns the text.
    pub fn into_inner(self) -> String {
        self.text
    }
}

impl fmt::Display for Bounded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl Serialize for Bounded {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Bounded", 2)?;
        state.serialize_field("text", &self.text)?;
        state.serialize_field("untrusted", &true)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for Bounded {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // `untrusted` is accepted and ignored: no input can mark a value trusted.
        #[derive(Deserialize)]
        struct Wire {
            text: String,
            #[serde(default, rename = "untrusted")]
            _untrusted: Option<bool>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.text, Self::MAX_CHARS).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for Bounded {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Bounded".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "description": "Untrusted, length-limited text.",
            "properties": {
                "text": { "type": "string", "maxLength": Self::MAX_CHARS },
                "untrusted": { "const": true }
            },
            "required": ["text", "untrusted"]
        })
    }
}

/// Why a string cannot be [`Bounded`] (spec 3.5 §7.10).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BoundedError {
    /// The text is longer than the limit.
    #[error(
        "text has {chars} characters, over the limit of {max}; truncate it with `Bounded::truncated`"
    )]
    TooLong {
        /// The text's length in characters.
        chars: usize,
        /// The limit.
        max: usize,
    },
    /// The limit is above [`Bounded::MAX_CHARS`].
    #[error("limit {max} is above the ceiling of {} characters", Bounded::MAX_CHARS)]
    LimitTooLarge {
        /// The requested limit.
        max: usize,
    },
}

open_enum! {
    /// A channel a person or agent reaches the site, or the owner, on (spec 3.5 §7.10,
    /// §7.13).
    pub enum Channel {
        /// A web page or form.
        Web => "web",
        /// The on-site chat.
        Chat => "chat",
        /// Email.
        Email => "email",
        /// SMS.
        Sms => "sms",
        /// Voice.
        Voice => "voice",
        /// WhatsApp.
        Whatsapp => "whatsapp",
        /// Telegram.
        Telegram => "telegram",
        /// Slack.
        Slack => "slack",
        /// Discord.
        Discord => "discord",
        /// Matrix.
        Matrix => "matrix",
        /// A social network.
        Social => "social",
        /// A2A.
        A2a => "a2a",
        /// MCP.
        Mcp => "mcp",
        /// The HTTP capability binding.
        Http => "http",
        @other
        /// A channel this version does not name.
        Other(String),
    }
}

/// A reference to a lead record in the back office's store (spec 3.5 §7.10).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct LeadRef(pub String);

/// A reference to a consent receipt by id (spec 3.5 §7.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ConsentReceiptRef(pub Uuid);

/// An opaque reference to a way of contacting someone; never the address itself
/// (spec 3.5 §7.10).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ContactRef(pub String);

/// A reference to a conversation thread (spec 3.5 §7.10).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ThreadRef(pub String);

/// A snake_case reason code (spec 3.5 §7.5, §7.10).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ReasonCode(pub String);

impl From<&str> for ReasonCode {
    fn from(code: &str) -> Self {
        Self(code.to_owned())
    }
}

/// How a visitor's or agent's attempt ended (spec 3.5 §7.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Outcome {
    /// It completed.
    Completed,
    /// It was held for the owner.
    Held,
    /// Policy refused it.
    Refused,
    /// It failed.
    Failed,
    /// The visitor or agent stopped.
    Abandoned,
}

/// What kind of inbound message arrived (spec 3.5 §7.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MessageCategory {
    /// A question or enquiry.
    Inquiry,
    /// A support request.
    Support,
    /// A complaint.
    Complaint,
    /// About an order.
    Order,
    /// About a booking.
    Booking,
    /// Feedback.
    Feedback,
    /// Spam.
    Spam,
    /// Anything else.
    Other,
}

/// What an abusive requester did (spec 3.5 §7.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AbuseKind {
    /// Exceeded its rate limits repeatedly.
    RateAbuse,
    /// Tried to instruct the site's agents.
    InjectionAttempt,
    /// Scraped against the site's usage terms.
    Scraping,
    /// Sent spam.
    Spam,
    /// Attempted fraud.
    Fraud,
    /// Presented an invalid signature or credential.
    InvalidCredential,
    /// Anything else.
    Other,
}
