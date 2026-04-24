use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Agent-to-agent message following the AWP communication format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aMessage {
    pub id: Uuid,
    pub sender: String,
    pub recipient: String,
    pub message_type: A2aMessageType,
    pub timestamp: DateTime<Utc>,
    pub payload: serde_json::Value,
}

/// Type of an agent-to-agent message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum A2aMessageType {
    Request,
    Response,
    Notification,
    Error,
}

/// AWP-specific typed message categories for agent routing.
///
/// These extend the generic [`A2aMessageType`] with domain-specific semantics
/// that enable typed dispatch in AWP gateways and agent meshes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwpMessageType {
    /// Visitor expressed purchase or service intent.
    VisitorIntentSignal,
    /// Content gap detected — missing or outdated information.
    ContentGapSignal,
    /// Payment intent lifecycle message.
    PaymentIntent,
    /// Escalation to human support.
    SupportEscalation,
    /// Review or feedback signal from a platform.
    ReviewSignal,
    /// Operational proposal (inventory, scheduling, etc.).
    OperationsProposal,
    /// Invoke a declared capability on a remote agent.
    InvokeCapability,
    /// Request UI rendering (dual-surface: HTML for humans, JSON-LD for agents).
    RenderUi,
    /// Proactive outbound message (follow-up, notification).
    OutboundTrigger,
}

impl std::fmt::Display for AwpMessageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VisitorIntentSignal => write!(f, "visitor_intent_signal"),
            Self::ContentGapSignal => write!(f, "content_gap_signal"),
            Self::PaymentIntent => write!(f, "payment_intent"),
            Self::SupportEscalation => write!(f, "support_escalation"),
            Self::ReviewSignal => write!(f, "review_signal"),
            Self::OperationsProposal => write!(f, "operations_proposal"),
            Self::InvokeCapability => write!(f, "invoke_capability"),
            Self::RenderUi => write!(f, "render_ui"),
            Self::OutboundTrigger => write!(f, "outbound_trigger"),
        }
    }
}

/// AWP-typed agent-to-agent message with domain-specific routing.
///
/// Unlike [`A2aMessage`] which uses generic request/response types, this
/// carries an [`AwpMessageType`] for typed dispatch in AWP gateways.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwpTypedMessage {
    pub id: Uuid,
    pub sender: String,
    pub recipient: String,
    pub awp_type: AwpMessageType,
    pub timestamp: DateTime<Utc>,
    pub payload: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a2a_message_serde_round_trip() {
        let msg = A2aMessage {
            id: Uuid::now_v7(),
            sender: "agent-a".to_string(),
            recipient: "agent-b".to_string(),
            message_type: A2aMessageType::Request,
            timestamp: Utc::now(),
            payload: serde_json::json!({"action": "greet"}),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: A2aMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_message_type_serde() {
        for mt in [
            A2aMessageType::Request,
            A2aMessageType::Response,
            A2aMessageType::Notification,
            A2aMessageType::Error,
        ] {
            let json = serde_json::to_string(&mt).unwrap();
            let deserialized: A2aMessageType = serde_json::from_str(&json).unwrap();
            assert_eq!(mt, deserialized);
        }
    }

    #[test]
    fn test_message_type_lowercase() {
        assert_eq!(serde_json::to_string(&A2aMessageType::Request).unwrap(), "\"request\"");
        assert_eq!(serde_json::to_string(&A2aMessageType::Response).unwrap(), "\"response\"");
        assert_eq!(
            serde_json::to_string(&A2aMessageType::Notification).unwrap(),
            "\"notification\""
        );
        assert_eq!(serde_json::to_string(&A2aMessageType::Error).unwrap(), "\"error\"");
    }

    #[test]
    fn test_awp_message_type_serde_round_trip() {
        let types = [
            AwpMessageType::VisitorIntentSignal,
            AwpMessageType::ContentGapSignal,
            AwpMessageType::PaymentIntent,
            AwpMessageType::SupportEscalation,
            AwpMessageType::ReviewSignal,
            AwpMessageType::OperationsProposal,
            AwpMessageType::InvokeCapability,
            AwpMessageType::RenderUi,
            AwpMessageType::OutboundTrigger,
        ];
        for mt in types {
            let json = serde_json::to_string(&mt).unwrap();
            let deserialized: AwpMessageType = serde_json::from_str(&json).unwrap();
            assert_eq!(mt, deserialized);
        }
    }

    #[test]
    fn test_awp_message_type_snake_case() {
        assert_eq!(
            serde_json::to_string(&AwpMessageType::VisitorIntentSignal).unwrap(),
            "\"visitor_intent_signal\""
        );
        assert_eq!(
            serde_json::to_string(&AwpMessageType::PaymentIntent).unwrap(),
            "\"payment_intent\""
        );
        assert_eq!(serde_json::to_string(&AwpMessageType::RenderUi).unwrap(), "\"render_ui\"");
    }

    #[test]
    fn test_awp_message_type_display() {
        assert_eq!(AwpMessageType::VisitorIntentSignal.to_string(), "visitor_intent_signal");
        assert_eq!(AwpMessageType::SupportEscalation.to_string(), "support_escalation");
        assert_eq!(AwpMessageType::OutboundTrigger.to_string(), "outbound_trigger");
    }

    #[test]
    fn test_awp_typed_message_serde_round_trip() {
        let msg = AwpTypedMessage {
            id: Uuid::now_v7(),
            sender: "visitor-agent".to_string(),
            recipient: "payment-agent".to_string(),
            awp_type: AwpMessageType::PaymentIntent,
            timestamp: Utc::now(),
            payload: serde_json::json!({"sku": "WIDGET-001", "amount": 2500}),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: AwpTypedMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }
}
