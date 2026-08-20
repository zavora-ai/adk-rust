//! Vertex AI resource-name parsing, formatting, and scope validation.

use std::fmt;

/// A parsed `projects/*/locations/*/reasoningEngines/*` resource name.
///
/// # Example
///
/// ```rust
/// use adk_gcp::VertexResourceName;
///
/// let name = VertexResourceName::new("my-project", "us-central1", "4242");
/// assert_eq!(
///     name.to_string(),
///     "projects/my-project/locations/us-central1/reasoningEngines/4242",
/// );
///
/// let parsed = VertexResourceName::parse(&name.to_string()).unwrap();
/// assert_eq!(parsed, name);
/// assert!(parsed.has_canonical_engine_id());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VertexResourceName {
    project_id: String,
    location: String,
    engine_id: String,
}

impl VertexResourceName {
    /// Creates a resource name from its parts.
    pub fn new(
        project_id: impl Into<String>,
        location: impl Into<String>,
        engine_id: impl Into<String>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            location: location.into(),
            engine_id: engine_id.into(),
        }
    }

    /// Parses a full `projects/*/locations/*/reasoningEngines/*` name.
    ///
    /// Returns `None` when the shape does not match, any segment is empty,
    /// or the name carries traversal or scheme characters (`..`, `://`).
    pub fn parse(name: &str) -> Option<Self> {
        if name.contains("://") || name.contains("..") {
            return None;
        }
        let mut segments = name.split('/');
        let (
            Some("projects"),
            Some(project_id),
            Some("locations"),
            Some(location),
            Some("reasoningEngines"),
            Some(engine_id),
            None,
        ) = (
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
        )
        else {
            return None;
        };
        if project_id.is_empty() || location.is_empty() || engine_id.is_empty() {
            return None;
        }
        Some(Self::new(project_id, location, engine_id))
    }

    /// The project ID segment.
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// The location segment.
    pub fn location(&self) -> &str {
        &self.location
    }

    /// The reasoning-engine ID segment.
    pub fn engine_id(&self) -> &str {
        &self.engine_id
    }

    /// Whether the engine ID is the canonical numeric form.
    pub fn has_canonical_engine_id(&self) -> bool {
        is_canonical_reasoning_engine_id(&self.engine_id)
    }

    /// The parent `projects/*/locations/*` prefix.
    pub fn parent(&self) -> String {
        format!("projects/{}/locations/{}", self.project_id, self.location)
    }
}

impl fmt::Display for VertexResourceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "projects/{}/locations/{}/reasoningEngines/{}",
            self.project_id, self.location, self.engine_id,
        )
    }
}

/// Whether `value` is a canonical numeric reasoning-engine ID.
///
/// Canonical IDs are all-ASCII digits without leading zeros (`"0"` itself
/// is canonical), matching the form the Vertex AI API mints.
///
/// # Example
///
/// ```rust
/// use adk_gcp::is_canonical_reasoning_engine_id;
///
/// assert!(is_canonical_reasoning_engine_id("4242"));
/// assert!(is_canonical_reasoning_engine_id("0"));
/// assert!(!is_canonical_reasoning_engine_id("042"));
/// assert!(!is_canonical_reasoning_engine_id("my-engine"));
/// ```
pub fn is_canonical_reasoning_engine_id(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

/// Whether `name` belongs to the given project and location scope.
///
/// Accepts names starting with `projects/{project_id}/locations/{location}/`
/// that carry no traversal or scheme characters, so a compromised or buggy
/// server cannot redirect follow-up requests (operation polls, deletes)
/// outside the configured scope.
pub fn is_scoped_resource_name(name: &str, project_id: &str, location: &str) -> bool {
    let prefix = format!("projects/{project_id}/locations/{location}/");
    name.starts_with(&prefix) && !name.contains("://") && !name.contains("..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format_round_trip() {
        let text = "projects/my-project/locations/us-central1/reasoningEngines/4242";
        let name = VertexResourceName::parse(text).unwrap();
        assert_eq!(name, VertexResourceName::new("my-project", "us-central1", "4242"),);
        assert_eq!(name.to_string(), text);
        assert_eq!(name.parent(), "projects/my-project/locations/us-central1");
    }

    #[test]
    fn parse_rejects_malformed_names() {
        let rejected = [
            "",
            "projects/p/locations/l",
            "projects/p/locations/l/reasoningEngines",
            "projects/p/locations/l/reasoningEngines/",
            "projects//locations/l/reasoningEngines/1",
            "projects/p/locations//reasoningEngines/1",
            "projects/p/locations/l/reasoningEngines/1/memories/2",
            "projects/p/locations/l/somethingElse/1",
            "https://evil.example/projects/p/locations/l/reasoningEngines/1",
            "projects/p/locations/l/reasoningEngines/../1",
        ];
        for name in rejected {
            assert!(VertexResourceName::parse(name).is_none(), "accepted {name:?}");
        }
    }

    #[test]
    fn canonical_engine_ids() {
        assert!(is_canonical_reasoning_engine_id("1"));
        assert!(is_canonical_reasoning_engine_id("0"));
        assert!(is_canonical_reasoning_engine_id("123456789"));
        assert!(!is_canonical_reasoning_engine_id(""));
        assert!(!is_canonical_reasoning_engine_id("01"));
        assert!(!is_canonical_reasoning_engine_id("12a"));
        assert!(!is_canonical_reasoning_engine_id("-1"));
    }

    #[test]
    fn scope_validation_pins_project_and_location() {
        assert!(is_scoped_resource_name(
            "projects/p/locations/l/reasoningEngines/1/operations/2",
            "p",
            "l",
        ));
        assert!(!is_scoped_resource_name(
            "projects/other/locations/l/reasoningEngines/1/operations/2",
            "p",
            "l",
        ));
        assert!(!is_scoped_resource_name(
            "projects/p/locations/l/../../other/operations/2",
            "p",
            "l",
        ));
        assert!(!is_scoped_resource_name("https://projects/p/locations/l/operations/2", "p", "l",));
    }
}
