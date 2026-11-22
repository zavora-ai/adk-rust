//! The perceiver, reasoner and verifier roles of a multimodal team (spec 3.7 §7.12).
//!
//! The roles are portable data: a capability each binding must advertise, and the exact
//! contract of the coordinator's edge to it. What a role may hold — a perceiver's tools that
//! never leave the device, a reasoner's images that only the egress gate released — is
//! enforced where tools and images are known, by `adk-vision`'s role builders and
//! `adk-harness`'s member presets; the team carries the contracts every binding is held to.
//!
//! | Role | Capability | Edge input | Edge output |
//! |---|---|---|---|
//! | Perceiver | [`PERCEIVER_CAPABILITY`] | [`perceiver_input_schema`]: an image and what to look for | Free-form regions |
//! | Reasoner | [`REASONER_CAPABILITY`] | [`reasoner_input_schema`]: a question, its verdicts, a `RegionSet`, measurements and released crop digests — no pixels | [`opinion_schema`] |
//! | Verifier | [`VERIFIER_CAPABILITY`] | [`verifier_input_schema`]: a claim, its regions and the source image's digest | [`opinion_schema`] |
//!
//! Every edge is isolated with no history, so a member sees only the request its contract
//! admits; a reasoner request that carries anything but those fields, such as an inline image,
//! is refused before the reasoner runs.

use serde_json::{Value, json};

use super::{
    RelationshipKind, RelationshipPolicy, TeamContextPolicy, TeamHistoryPolicy, TeamMemberSpec,
    TeamPolicy, TeamRelationship, TeamSpec,
};

/// The capability a perceiver binding advertises: it perceives on the device, with a local
/// model or none, and holds no tool with a remote destination.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_agent::PERCEIVER_CAPABILITY, "vision.perceive.local");
/// ```
pub const PERCEIVER_CAPABILITY: &str = "vision.perceive.local";

/// The capability a reasoner binding advertises: it reasons over regions, measurements and
/// crops the egress gate released, and holds no perception tool.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_agent::REASONER_CAPABILITY, "vision.reason.released");
/// ```
pub const REASONER_CAPABILITY: &str = "vision.reason.released";

/// The capability a verifier binding advertises: it re-checks a claim against the pixels with
/// local perception.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_agent::VERIFIER_CAPABILITY, "vision.verify.pixels");
/// ```
pub const VERIFIER_CAPABILITY: &str = "vision.verify.pixels";

/// The pattern of an image digest on a role edge.
const DIGEST_PATTERN: &str = "^sha256:[0-9a-f]{64}$";

/// The coordinator's request to a perceiver: the image, by digest or artifact name, and
/// optionally what to look for.
///
/// # Example
///
/// ```rust
/// let schema = adk_agent::perceiver_input_schema();
/// assert_eq!(schema["required"], serde_json::json!(["image"]));
/// ```
pub fn perceiver_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["image"],
        "properties": {
            "image": { "type": "string", "minLength": 1, "description": "A sha256: digest or an artifact name." },
            "question": { "type": "string" },
            "classes": { "type": "array", "items": { "type": "string", "minLength": 1 } }
        }
    })
}

/// The coordinator's request to a reasoner: a question, the perceiver's `RegionSet`,
/// measurements, and the digests of crops the egress gate released. It has no field that
/// can carry pixels.
///
/// # Example
///
/// ```rust
/// let schema = adk_agent::reasoner_input_schema();
/// assert_eq!(schema["additionalProperties"], false);
/// assert!(schema["properties"].get("image").is_none());
/// ```
pub fn reasoner_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["question", "regions"],
        "properties": {
            "question": { "type": "string", "minLength": 1 },
            "verdicts": {
                "type": "array",
                "items": { "type": "string", "minLength": 1 },
                "minItems": 2,
                "description": "The verdicts the answer must be one of."
            },
            "regions": { "type": "object", "description": "A RegionSet in source pixels." },
            "measurements": { "type": "array", "items": { "type": "object" } },
            "crops": {
                "type": "array",
                "items": { "type": "string", "pattern": DIGEST_PATTERN },
                "description": "Egress digests of crops the egress gate released."
            }
        }
    })
}

/// The coordinator's request to a verifier: the claim to re-check, its regions and the
/// digest of the source image on the device.
///
/// # Example
///
/// ```rust
/// let schema = adk_agent::verifier_input_schema();
/// assert_eq!(schema["required"], serde_json::json!(["claim", "image"]));
/// ```
pub fn verifier_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["claim", "image"],
        "properties": {
            "claim": opinion_schema(),
            "image": { "type": "string", "pattern": DIGEST_PATTERN },
            "regions": { "type": "object", "description": "A RegionSet in source pixels." },
            "question": { "type": "string" }
        }
    })
}

/// What a reasoner or verifier answers: a verdict, a confidence, the regions it rests on and
/// why.
///
/// # Example
///
/// ```rust
/// let schema = adk_agent::opinion_schema();
/// assert_eq!(schema["required"], serde_json::json!(["verdict", "confidence"]));
/// ```
pub fn opinion_schema() -> Value {
    json!({
        "type": "object",
        "required": ["verdict", "confidence"],
        "properties": {
            "verdict": { "type": "string", "minLength": 1 },
            "label": { "type": "string" },
            "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
            "regions": { "type": "object" },
            "rationale": { "type": "string" }
        }
    })
}

/// The edge contract of one role: isolated, no history, the role's input schema and, for a
/// reasoner or verifier, the opinion schema on what it returns.
fn role_policy(input: Value, output: Option<Value>) -> RelationshipPolicy {
    RelationshipPolicy {
        input_schema: Some(input),
        output_schema: output,
        context: Some(TeamContextPolicy::Isolated),
        history: TeamHistoryPolicy::None,
        ..RelationshipPolicy::default()
    }
}

/// Lowers the perceive, reason, verify architecture; see
/// [`TeamArchitectureTemplate::PerceiveReasonVerify`](super::TeamArchitectureTemplate::PerceiveReasonVerify).
pub(super) fn lower(
    name: &str,
    coordinator: &str,
    perceiver: &str,
    reasoner: &str,
    verifier: &str,
    policy: &TeamPolicy,
) -> TeamSpec {
    TeamSpec {
        name: name.to_string(),
        description: "Perceiver, reasoner and verifier team compiled from a portable architecture \
                      template"
            .to_string(),
        coordinator: coordinator.to_string(),
        members: vec![
            TeamMemberSpec::new(coordinator).with_description(
                "Asks the perceiver, reasoner and verifier, and escalates a disagreement to a \
                 person with both opinions and the evidence.",
            ),
            TeamMemberSpec::new(perceiver)
                .with_description(
                    "Perceives on the device with a local model or none; holds no tool with a \
                     remote destination.",
                )
                .with_capabilities([PERCEIVER_CAPABILITY]),
            TeamMemberSpec::new(reasoner)
                .with_description(
                    "Reasons over regions, measurements and redacted crops the egress gate \
                     released; holds no perception tool.",
                )
                .with_capabilities([REASONER_CAPABILITY]),
            TeamMemberSpec::new(verifier)
                .with_description(
                    "Re-checks a claim against the pixels with local perception, by default on \
                     a different provider from the reasoner.",
                )
                .with_capabilities([VERIFIER_CAPABILITY]),
        ],
        relationships: vec![
            TeamRelationship::new(coordinator, perceiver, RelationshipKind::Delegate)
                .with_policy(role_policy(perceiver_input_schema(), None)),
            TeamRelationship::new(coordinator, reasoner, RelationshipKind::Delegate)
                .with_policy(role_policy(reasoner_input_schema(), Some(opinion_schema()))),
            TeamRelationship::new(coordinator, verifier, RelationshipKind::Delegate)
                .with_policy(role_policy(verifier_input_schema(), Some(opinion_schema()))),
        ],
        policy: policy.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TeamArchitectureTemplate;

    fn template() -> TeamArchitectureTemplate {
        TeamArchitectureTemplate::PerceiveReasonVerify {
            name: "inspector".to_string(),
            coordinator: "lead".to_string(),
            perceiver: "perceiver".to_string(),
            reasoner: "reasoner".to_string(),
            verifier: "verifier".to_string(),
            policy: TeamPolicy::default(),
        }
    }

    #[test]
    fn lowers_to_three_isolated_role_edges() {
        let spec = template().lower().unwrap();
        assert_eq!(spec.coordinator, "lead");
        let capabilities: Vec<(&str, Vec<String>)> = spec
            .members
            .iter()
            .map(|member| (member.name.as_str(), member.required_capabilities.clone()))
            .collect();
        assert_eq!(
            capabilities,
            vec![
                ("lead", vec![]),
                ("perceiver", vec![PERCEIVER_CAPABILITY.to_string()]),
                ("reasoner", vec![REASONER_CAPABILITY.to_string()]),
                ("verifier", vec![VERIFIER_CAPABILITY.to_string()]),
            ]
        );
        for edge in &spec.relationships {
            assert_eq!(edge.from, "lead");
            assert_eq!(edge.kind, RelationshipKind::Delegate);
            assert_eq!(edge.policy.context, Some(TeamContextPolicy::Isolated));
            assert_eq!(edge.policy.history, TeamHistoryPolicy::None);
        }
        assert_eq!(spec.relationships[1].policy.input_schema, Some(reasoner_input_schema()));
        assert_eq!(spec.relationships[1].policy.output_schema, Some(opinion_schema()));
    }

    #[test]
    fn a_reasoner_request_cannot_carry_pixels() {
        let validator = jsonschema::validator_for(&reasoner_input_schema()).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        let admitted = json!({
            "question": "Is the part defective?",
            "regions": { "image": digest, "regions": [] },
            "crops": [digest],
        });
        assert!(validator.is_valid(&admitted));
        let inline_image = json!({
            "question": "Is the part defective?",
            "regions": {},
            "image": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGBgAAAABQABpfZFQAAAAABJRU5ErkJggg==",
        });
        assert!(!validator.is_valid(&inline_image));
        let base64_as_crop = json!({
            "question": "Is the part defective?",
            "regions": {},
            "crops": ["iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJ"],
        });
        assert!(!validator.is_valid(&base64_as_crop));
    }

    #[test]
    fn a_role_named_twice_is_refused() {
        let error = TeamArchitectureTemplate::PerceiveReasonVerify {
            name: "inspector".to_string(),
            coordinator: "lead".to_string(),
            perceiver: "same".to_string(),
            reasoner: "same".to_string(),
            verifier: "verifier".to_string(),
            policy: TeamPolicy::default(),
        }
        .lower()
        .unwrap_err();
        assert_eq!(error, crate::TeamError::DuplicateMember("same".to_string()));
    }

    #[test]
    fn the_template_round_trips_as_data() {
        let value = serde_json::to_value(template()).unwrap();
        assert_eq!(value["architecture"], "perceiveReasonVerify");
        let back: TeamArchitectureTemplate = serde_json::from_value(value).unwrap();
        assert_eq!(back, template());
    }
}
