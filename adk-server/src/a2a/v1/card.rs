//! A2A v1.0.0 agent card builder and caching.
//!
//! Provides [`CachedAgentCard`] for serving the agent card with HTTP caching
//! headers (ETag, Last-Modified) and conditional request handling.
//!
//! Also provides [`build_v1_agent_card`] to construct an
//! [`a2a_protocol_types::AgentCard`] from ADK agent metadata.

use a2a_protocol_types::{AgentCapabilities, AgentCard, AgentInterface, AgentSkill};
use chrono::{DateTime, Utc};

/// Cached agent card with ETag, Last-Modified, and pre-serialized JSON.
#[derive(Debug, Clone)]
pub struct CachedAgentCard {
    /// The agent card.
    pub card: AgentCard,
    /// Pre-serialized card JSON bytes.
    pub card_json: Vec<u8>,
    /// Deterministic hash of the serialized card JSON, used as ETag.
    pub etag: String,
    /// Timestamp of the last card change.
    pub last_modified: DateTime<Utc>,
}

impl CachedAgentCard {
    /// Creates a new cached agent card, computing the ETag and serialized JSON.
    pub fn new(card: AgentCard) -> Self {
        let card_json = serde_json::to_vec(&card).unwrap_or_default();
        let etag = compute_etag(&card_json);
        Self { card, card_json, etag, last_modified: Utc::now() }
    }

    /// Returns `true` if the given `If-None-Match` value matches the current ETag.
    ///
    /// Supports both quoted (`"abc123"`) and unquoted (`abc123`) ETag values.
    pub fn matches_etag(&self, if_none_match: &str) -> bool {
        let trimmed = if_none_match.trim();
        // Handle wildcard
        if trimmed == "*" {
            return true;
        }
        // Strip surrounding quotes if present
        let unquoted = trimmed.trim_matches('"');
        unquoted == self.etag
    }

    /// Returns `true` if the card was modified after the given timestamp.
    pub fn modified_since(&self, if_modified_since: &DateTime<Utc>) -> bool {
        self.last_modified > *if_modified_since
    }

    /// Updates the cached card, recomputing the ETag, serialized JSON, and
    /// last_modified timestamp.
    pub fn update(&mut self, card: AgentCard) {
        let card_json = serde_json::to_vec(&card).unwrap_or_default();
        self.etag = compute_etag(&card_json);
        self.card_json = card_json;
        self.card = card;
        self.last_modified = Utc::now();
    }
}

/// Compute a deterministic ETag string from serialized JSON bytes.
///
/// Uses `DefaultHasher` (SipHash) which is deterministic within a single
/// process build. This is sufficient for ETag purposes since the server
/// recomputes on startup.
fn compute_etag(json_bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    json_bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Build an [`AgentCard`] from ADK agent metadata.
///
/// Populates `supported_interfaces` with a JSON-RPC binding at the given URL
/// with `protocolVersion: "1.0"`, sets `default_input_modes` and
/// `default_output_modes` to `["text/plain"]`, and uses the provided capabilities.
pub fn build_v1_agent_card(
    name: &str,
    description: &str,
    url: &str,
    version: &str,
    skills: Vec<AgentSkill>,
    capabilities: AgentCapabilities,
) -> AgentCard {
    AgentCard {
        name: name.to_string(),
        url: Some(url.to_string()),
        description: description.to_string(),
        version: version.to_string(),
        supported_interfaces: vec![AgentInterface {
            url: url.to_string(),
            protocol_binding: "JSONRPC".to_string(),
            protocol_version: "1.0".to_string(),
            tenant: None,
        }],
        default_input_modes: vec!["text/plain".to_string()],
        default_output_modes: vec!["text/plain".to_string()],
        skills,
        capabilities,
        provider: None,
        icon_url: None,
        documentation_url: None,
        security_schemes: None,
        security_requirements: None,
        signatures: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_skill(id: &str, name: &str) -> AgentSkill {
        AgentSkill {
            id: id.to_string(),
            name: name.to_string(),
            description: format!("{name} skill"),
            tags: vec!["test".to_string()],
            examples: None,
            input_modes: None,
            output_modes: None,
            security_requirements: None,
        }
    }

    fn make_card(name: &str) -> AgentCard {
        build_v1_agent_card(
            name,
            "A test agent",
            "http://localhost:8080",
            "1.0.0",
            vec![make_skill("echo", "Echo")],
            AgentCapabilities::default(),
        )
    }

    #[test]
    fn etag_is_deterministic() {
        let card1 = make_card("agent-a");
        let card2 = make_card("agent-a");
        let cached1 = CachedAgentCard::new(card1);
        let cached2 = CachedAgentCard::new(card2);
        assert_eq!(cached1.etag, cached2.etag, "same card should produce same ETag");
    }

    #[test]
    fn different_cards_produce_different_etags() {
        let cached_a = CachedAgentCard::new(make_card("agent-a"));
        let cached_b = CachedAgentCard::new(make_card("agent-b"));
        assert_ne!(cached_a.etag, cached_b.etag, "different cards should produce different ETags");
    }

    #[test]
    fn matches_etag_unquoted() {
        let cached = CachedAgentCard::new(make_card("agent"));
        assert!(cached.matches_etag(&cached.etag));
    }

    #[test]
    fn matches_etag_quoted() {
        let cached = CachedAgentCard::new(make_card("agent"));
        let quoted = format!("\"{}\"", cached.etag);
        assert!(cached.matches_etag(&quoted));
    }

    #[test]
    fn matches_etag_wildcard() {
        let cached = CachedAgentCard::new(make_card("agent"));
        assert!(cached.matches_etag("*"));
    }

    #[test]
    fn matches_etag_mismatch() {
        let cached = CachedAgentCard::new(make_card("agent"));
        assert!(!cached.matches_etag("not-a-real-etag"));
    }

    #[test]
    fn modified_since_returns_true_for_older_timestamp() {
        let cached = CachedAgentCard::new(make_card("agent"));
        let past = cached.last_modified - Duration::seconds(60);
        assert!(cached.modified_since(&past));
    }

    #[test]
    fn modified_since_returns_false_for_future_timestamp() {
        let cached = CachedAgentCard::new(make_card("agent"));
        let future = cached.last_modified + Duration::seconds(60);
        assert!(!cached.modified_since(&future));
    }

    #[test]
    fn modified_since_returns_false_for_exact_timestamp() {
        let cached = CachedAgentCard::new(make_card("agent"));
        assert!(!cached.modified_since(&cached.last_modified));
    }

    #[test]
    fn update_changes_etag_and_last_modified() {
        let mut cached = CachedAgentCard::new(make_card("agent-old"));
        let old_etag = cached.etag.clone();
        let old_modified = cached.last_modified;

        // Small sleep to ensure timestamp differs
        std::thread::sleep(std::time::Duration::from_millis(10));

        cached.update(make_card("agent-new"));
        assert_ne!(cached.etag, old_etag, "ETag should change after update");
        assert!(cached.last_modified >= old_modified, "last_modified should be updated");
        assert_eq!(cached.card.name, "agent-new");
    }

    #[test]
    fn update_preserves_etag_for_same_card() {
        let card = make_card("agent");
        let mut cached = CachedAgentCard::new(card.clone());
        let original_etag = cached.etag.clone();

        cached.update(card);
        assert_eq!(cached.etag, original_etag, "same card content should produce same ETag");
    }

    #[test]
    fn card_json_is_valid_json() {
        let cached = CachedAgentCard::new(make_card("agent"));
        let parsed: serde_json::Value =
            serde_json::from_slice(&cached.card_json).expect("card_json should be valid JSON");
        assert_eq!(parsed["name"], "agent");
    }

    #[test]
    fn build_v1_agent_card_populates_supported_interfaces() {
        let card = build_v1_agent_card(
            "my-agent",
            "My agent",
            "http://example.com",
            "2.0.0",
            vec![make_skill("s1", "Skill1")],
            AgentCapabilities::default(),
        );
        assert_eq!(card.supported_interfaces.len(), 1);
        assert_eq!(card.supported_interfaces[0].protocol_binding, "JSONRPC");
        assert_eq!(card.supported_interfaces[0].protocol_version, "1.0");
        assert_eq!(card.supported_interfaces[0].url, "http://example.com");
    }

    #[test]
    fn build_v1_agent_card_sets_default_modes() {
        let card = build_v1_agent_card(
            "my-agent",
            "My agent",
            "http://example.com",
            "1.0.0",
            vec![make_skill("s1", "Skill1")],
            AgentCapabilities::default(),
        );
        assert_eq!(card.default_input_modes, vec!["text/plain"]);
        assert_eq!(card.default_output_modes, vec!["text/plain"]);
    }

    #[test]
    fn build_v1_agent_card_sets_version() {
        let card = build_v1_agent_card(
            "my-agent",
            "My agent",
            "http://example.com",
            "3.5.1",
            vec![make_skill("s1", "Skill1")],
            AgentCapabilities::default(),
        );
        assert_eq!(card.version, "3.5.1");
    }

    #[test]
    fn build_v1_agent_card_passes_through_capabilities() {
        let mut caps = AgentCapabilities::default();
        caps.streaming = Some(true);
        caps.push_notifications = Some(true);
        let card = build_v1_agent_card(
            "my-agent",
            "My agent",
            "http://example.com",
            "1.0.0",
            vec![make_skill("s1", "Skill1")],
            caps.clone(),
        );
        assert_eq!(card.capabilities.streaming, Some(true));
        assert_eq!(card.capabilities.push_notifications, Some(true));
    }
}
