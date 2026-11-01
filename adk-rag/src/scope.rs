//! The partition key every vector store operation names.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{RagError, Result};

/// The partition a vector store operation reads, writes, or deletes.
///
/// Every [`VectorStore`](crate::VectorStore) operation names a scope, and a stored chunk is
/// identified by its collection, its scope, and its chunk ID together. Two tenants that
/// share a collection and ingest the same document ID keep separate chunks, and a search in
/// one scope never returns another scope's chunks: the scope is applied inside the backend's
/// search, before the top `k` are chosen.
///
/// A scope is an opaque key of 1 to [`Scope::MAX_LEN`] bytes without control characters.
/// [`Scope::from_parts`] builds one from several parts, such as an application and a
/// tenant, and [`Scope::single_tenant`] is the scope of an application with one tenant.
///
/// # Example
///
/// ```rust
/// use adk_rag::Scope;
///
/// let tenant = Scope::from_parts(["support-app", "tenant/42"])?;
/// assert_eq!(tenant.as_str(), "support-app/tenant%2F42");
///
/// let raw = Scope::new("org-7")?;
/// assert_eq!(raw.to_string(), "org-7");
/// assert!(Scope::new("").is_err());
/// # Ok::<(), adk_rag::RagError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Scope(String);

impl Scope {
    /// The longest scope key, in bytes.
    pub const MAX_LEN: usize = 256;

    /// The key of [`Scope::single_tenant`]. [`Scope::from_parts`] never produces it, because
    /// it percent-encodes `:`.
    pub const SINGLE_TENANT_KEY: &'static str = ":single-tenant";

    /// Creates a scope from an opaque key.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::InvalidScope`] when `key` is empty, longer than
    /// [`Scope::MAX_LEN`] bytes, or contains a control character.
    pub fn new(key: impl Into<String>) -> Result<Self> {
        let key = key.into();
        if key.is_empty() || key.len() > Self::MAX_LEN {
            return Err(RagError::InvalidScope(format!(
                "a scope key is 1 to {} bytes, got {}",
                Self::MAX_LEN,
                key.len()
            )));
        }
        if key.chars().any(char::is_control) {
            return Err(RagError::InvalidScope(
                "a scope key contains no control characters".to_string(),
            ));
        }
        Ok(Self(key))
    }

    /// Creates a scope from several parts, such as an application and a tenant.
    ///
    /// Each part is percent-encoded, leaving only ASCII letters, digits, `-`, `.`, `_`, and
    /// `~` as they are, and the parts are joined with `/`. Distinct part lists therefore
    /// always give distinct scopes.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_rag::Scope;
    ///
    /// let a = Scope::from_parts(["app", "a/b"])?;
    /// let b = Scope::from_parts(["app", "a", "b"])?;
    /// assert_ne!(a, b);
    /// # Ok::<(), adk_rag::RagError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`RagError::InvalidScope`] when there are no parts, a part is empty, or the
    /// encoded key is longer than [`Scope::MAX_LEN`] bytes.
    pub fn from_parts<I, S>(parts: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut key = String::new();
        for (index, part) in parts.into_iter().enumerate() {
            let part = part.as_ref();
            if part.is_empty() {
                return Err(RagError::InvalidScope(format!("scope part {index} is empty")));
            }
            if index > 0 {
                key.push('/');
            }
            for byte in part.bytes() {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                    key.push(char::from(byte));
                } else {
                    key.push_str(&format!("%{byte:02X}"));
                }
            }
        }
        if key.is_empty() {
            return Err(RagError::InvalidScope("a scope needs at least one part".to_string()));
        }
        Self::new(key)
    }

    /// The scope of an application with a single tenant.
    ///
    /// Its key is [`Scope::SINGLE_TENANT_KEY`]. pgvector assigns it to rows stored before
    /// scopes existed.
    pub fn single_tenant() -> Self {
        Self(Self::SINGLE_TENANT_KEY.to_string())
    }

    /// The scope key.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A key that identifies `chunk_id` within this scope, for backends that address a record
    /// by one string.
    ///
    /// The key length-prefixes the scope, so distinct scope and chunk ID pairs never share a
    /// key.
    #[cfg(any(feature = "qdrant", feature = "surrealdb", feature = "agent-retrieval"))]
    pub(crate) fn record_key(&self, chunk_id: &str) -> String {
        format!("{}:{}{chunk_id}", self.0.len(), self.0)
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for Scope {
    type Error = RagError;

    fn try_from(key: String) -> Result<Self> {
        Self::new(key)
    }
}

impl From<Scope> for String {
    fn from(scope: Scope) -> Self {
        scope.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_empty_long_and_control_keys() {
        for key in [String::new(), "x".repeat(Scope::MAX_LEN + 1), "a\nb".into(), "a\0b".into()] {
            assert!(
                matches!(Scope::new(key.clone()), Err(RagError::InvalidScope(_))),
                "{key:?} is accepted"
            );
        }
        assert_eq!(
            Scope::new("x".repeat(Scope::MAX_LEN)).map(|s| s.as_str().len()).ok(),
            Some(256)
        );
    }

    #[test]
    fn from_parts_is_injective_and_never_yields_the_single_tenant_key() {
        let lists: [&[&str]; 6] =
            [&["a", "b"], &["a/b"], &["a%2Fb"], &["a", "b", "c"], &[":single-tenant"], &["a b"]];
        let keys: Vec<String> = lists
            .iter()
            .map(|parts| Scope::from_parts(parts.iter()).expect("valid parts").0)
            .collect();
        assert_eq!(
            keys,
            ["a/b", "a%2Fb", "a%252Fb", "a/b/c", "%3Asingle-tenant", "a%20b"].map(String::from)
        );
        assert!(Scope::from_parts(Vec::<&str>::new()).is_err());
        assert!(Scope::from_parts(["app", ""]).is_err());
    }

    #[cfg(any(feature = "qdrant", feature = "surrealdb", feature = "agent-retrieval"))]
    #[test]
    fn record_keys_of_distinct_pairs_differ() {
        let ab = Scope::new("ab").unwrap();
        let a = Scope::new("a").unwrap();
        assert_ne!(ab.record_key("c"), a.record_key("bc"));
        assert_eq!(ab.record_key("c"), "2:abc");
    }

    #[test]
    fn serde_round_trips_and_validates() {
        let scope = Scope::from_parts(["app", "t1"]).unwrap();
        let json = serde_json::to_string(&scope).unwrap();
        assert_eq!(json, "\"app/t1\"");
        assert_eq!(serde_json::from_str::<Scope>(&json).unwrap(), scope);
        assert!(serde_json::from_str::<Scope>("\"\"").is_err());
    }
}
