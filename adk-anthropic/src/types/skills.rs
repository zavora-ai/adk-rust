use serde::{Deserialize, Serialize};

/// A skill object returned by the Skills API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillObject {
    /// Unique skill identifier.
    pub id: String,
    /// Skill name.
    pub name: String,
    /// Skill description.
    pub description: String,
    /// Unix timestamp of creation.
    pub created_at: i64,
    /// Skill type (e.g. "custom" or "anthropic").
    #[serde(rename = "type")]
    pub skill_type: String,
}

/// A reference to a skill for use in message requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillRef {
    /// The ID of the skill to reference.
    pub skill_id: String,
}

impl SkillRef {
    /// Create a new `SkillRef` with the given skill ID.
    pub fn new(skill_id: impl Into<String>) -> Self {
        Self { skill_id: skill_id.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn skill_object_roundtrip() {
        let obj = SkillObject {
            id: "skill-abc".to_string(),
            name: "Code Review".to_string(),
            description: "Reviews code for quality".to_string(),
            created_at: 1700000000,
            skill_type: "custom".to_string(),
        };
        let json = serde_json::to_value(&obj).unwrap();
        let deserialized: SkillObject = serde_json::from_value(json).unwrap();
        assert_eq!(obj, deserialized);
    }

    #[test]
    fn skill_ref_serialization() {
        let skill_ref = SkillRef::new("skill-123");
        let json = serde_json::to_value(&skill_ref).unwrap();
        assert_eq!(json, json!({"skill_id": "skill-123"}));
    }
}
