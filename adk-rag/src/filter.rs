//! Portable filters over stored chunks.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::document::Chunk;
use crate::error::{RagError, Result};

/// The longest metadata key a filter can name, in bytes.
pub const MAX_FIELD_LEN: usize = 64;

/// A chunk field a [`Filter`] tests.
///
/// # Example
///
/// ```rust
/// use adk_rag::Field;
///
/// let section = Field::metadata("section");
/// assert_eq!(section, Field::Metadata("section".to_string()));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Field {
    /// A metadata entry. Its key must be declared filterable on the collection.
    Metadata(String),
    /// The chunk's document ID, which every chunk has.
    DocumentId,
    /// The chunk's source URI, which a chunk may lack.
    SourceUri,
    /// The chunk ID, which every chunk has.
    ChunkId,
}

impl Field {
    /// The metadata entry named `key`.
    pub fn metadata(key: impl Into<String>) -> Self {
        Self::Metadata(key.into())
    }

    /// Whether every chunk has this field.
    #[cfg(feature = "agent-retrieval")]
    pub(crate) fn always_present(&self) -> bool {
        match self {
            Self::DocumentId | Self::ChunkId => true,
            Self::Metadata(_) | Self::SourceUri => false,
        }
    }

    fn value<'a>(&self, chunk: &'a Chunk) -> Option<&'a str> {
        match self {
            Self::Metadata(key) => chunk.metadata.get(key).map(String::as_str),
            Self::DocumentId => Some(&chunk.document_id),
            Self::SourceUri => chunk.source_uri.as_deref(),
            Self::ChunkId => Some(&chunk.id),
        }
    }
}

/// A predicate over stored chunks, applied inside the vector store.
///
/// Every backend compiles a filter into its own query language with bound parameters or
/// typed expressions and applies it with the scope before the top `k` are chosen, so a
/// filtered search returns the nearest matching chunks rather than the matching part of the
/// global nearest. A backend that cannot express a filter fails with
/// [`RagError::UnsupportedFilter`] before any I/O; it never ignores a predicate or applies
/// part of one.
///
/// Values are strings. A field a chunk lacks matches no `Eq` or `In` and fails `Exists`, so
/// `Not(Eq(field, value))` matches chunks without the field.
///
/// # Example
///
/// ```rust
/// use adk_rag::{Chunk, Field, Filter};
///
/// let filter = Filter::and([
///     Filter::eq(Field::metadata("lang"), "en"),
///     !Filter::is_in(Field::DocumentId, ["draft-1", "draft-2"]),
/// ]);
///
/// let chunk = Chunk::new("guide_0", "guide", "text").with_metadata("lang", "en");
/// assert!(filter.matches(&chunk));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Filter {
    /// The field is present and equals the value.
    Eq(Field, String),
    /// The field is present and equals one of the values. The list is not empty.
    In(Field, Vec<String>),
    /// The field is present.
    Exists(Field),
    /// Every filter matches. The list is not empty.
    And(Vec<Filter>),
    /// At least one filter matches. The list is not empty.
    Or(Vec<Filter>),
    /// The filter does not match.
    Not(Box<Filter>),
}

impl Filter {
    /// Matches chunks whose `field` equals `value`.
    pub fn eq(field: Field, value: impl Into<String>) -> Self {
        Self::Eq(field, value.into())
    }

    /// Matches chunks whose `field` equals one of `values`.
    pub fn is_in<I, S>(field: Field, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::In(field, values.into_iter().map(Into::into).collect())
    }

    /// Matches chunks that have `field`.
    pub fn exists(field: Field) -> Self {
        Self::Exists(field)
    }

    /// Matches chunks every filter matches.
    pub fn and(filters: impl IntoIterator<Item = Filter>) -> Self {
        Self::And(filters.into_iter().collect())
    }

    /// Matches chunks at least one filter matches.
    pub fn or(filters: impl IntoIterator<Item = Filter>) -> Self {
        Self::Or(filters.into_iter().collect())
    }

    /// Whether `chunk` matches, by the semantics every backend implements.
    ///
    /// The in-memory store searches with this evaluator, and the shared contract compares
    /// every other backend against it.
    pub fn matches(&self, chunk: &Chunk) -> bool {
        match self {
            Self::Eq(field, value) => field.value(chunk) == Some(value.as_str()),
            Self::In(field, values) => {
                field.value(chunk).is_some_and(|actual| values.iter().any(|v| v == actual))
            }
            Self::Exists(field) => field.value(chunk).is_some(),
            Self::And(filters) => filters.iter().all(|filter| filter.matches(chunk)),
            Self::Or(filters) => filters.iter().any(|filter| filter.matches(chunk)),
            Self::Not(filter) => !filter.matches(chunk),
        }
    }

    /// Checks the filter's shape: every `And`, `Or`, and `In` list is non-empty, and every
    /// metadata key is 1 to [`MAX_FIELD_LEN`] ASCII letters, digits, and underscores,
    /// starting with a letter or underscore.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::UnsupportedFilter`] naming the first violation.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Eq(field, _) | Self::Exists(field) => validate_field(field),
            Self::In(field, values) => {
                if values.is_empty() {
                    return Err(unsupported("an `In` filter needs at least one value"));
                }
                validate_field(field)
            }
            Self::And(filters) | Self::Or(filters) => {
                if filters.is_empty() {
                    return Err(unsupported("an `And` or `Or` filter needs at least one filter"));
                }
                filters.iter().try_for_each(Filter::validate)
            }
            Self::Not(filter) => filter.validate(),
        }
    }

    /// Checks the filter's shape and that every metadata key it names is in `filterable`.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::UnsupportedFilter`] for a malformed filter or an undeclared key.
    pub fn validate_declared(&self, collection: &str, filterable: &BTreeSet<String>) -> Result<()> {
        self.validate()?;
        let mut undeclared = None;
        self.visit_fields(&mut |field| {
            if let Field::Metadata(key) = field
                && !filterable.contains(key)
                && undeclared.is_none()
            {
                undeclared = Some(key.clone());
            }
        });
        match undeclared {
            Some(key) => Err(unsupported(&format!(
                "metadata key `{key}` is not declared filterable on collection '{collection}'; \
                 create the collection with it in `CollectionSpec::with_filterable`"
            ))),
            None => Ok(()),
        }
    }

    /// Calls `visit` with every field the filter names.
    pub(crate) fn visit_fields(&self, visit: &mut impl FnMut(&Field)) {
        match self {
            Self::Eq(field, _) | Self::In(field, _) | Self::Exists(field) => visit(field),
            Self::And(filters) | Self::Or(filters) => {
                filters.iter().for_each(|filter| filter.visit_fields(visit));
            }
            Self::Not(filter) => filter.visit_fields(visit),
        }
    }
}

/// `!filter` matches chunks `filter` does not match.
impl std::ops::Not for Filter {
    type Output = Filter;

    fn not(self) -> Filter {
        Filter::Not(Box::new(self))
    }
}

/// Checks a metadata key against the rule every backend can store as a column or field name.
pub(crate) fn validate_field_name(key: &str) -> Result<()> {
    let valid = !key.is_empty()
        && key.len() <= MAX_FIELD_LEN
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !key.starts_with(|c: char| c.is_ascii_digit());
    if valid {
        Ok(())
    } else {
        Err(unsupported(&format!(
            "metadata key `{key}` cannot be filtered: use 1 to {MAX_FIELD_LEN} ASCII letters, \
             digits, or underscores, starting with a letter or underscore"
        )))
    }
}

fn validate_field(field: &Field) -> Result<()> {
    match field {
        Field::Metadata(key) => validate_field_name(key),
        Field::DocumentId | Field::SourceUri | Field::ChunkId => Ok(()),
    }
}

fn unsupported(reason: &str) -> RagError {
    RagError::UnsupportedFilter(reason.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk() -> Chunk {
        Chunk::new("doc_0", "doc", "text").with_metadata("lang", "en")
    }

    #[test]
    fn a_missing_field_matches_no_comparison_and_its_negation_matches() {
        let chunk = chunk();
        let tier = Field::metadata("tier");
        assert!(!Filter::eq(tier.clone(), "gold").matches(&chunk));
        assert!(!Filter::is_in(tier.clone(), ["gold"]).matches(&chunk));
        assert!(!Filter::exists(tier.clone()).matches(&chunk));
        assert!((!Filter::eq(tier, "gold")).matches(&chunk));
        assert!(!Filter::exists(Field::SourceUri).matches(&chunk));
        assert!(Filter::exists(Field::ChunkId).matches(&chunk));
    }

    #[test]
    fn combinators_follow_boolean_logic() {
        let chunk = chunk();
        let en = Filter::eq(Field::metadata("lang"), "en");
        let fr = Filter::eq(Field::metadata("lang"), "fr");
        assert!(Filter::and([en.clone(), Filter::eq(Field::DocumentId, "doc")]).matches(&chunk));
        assert!(!Filter::and([en.clone(), fr.clone()]).matches(&chunk));
        assert!(Filter::or([fr.clone(), en.clone()]).matches(&chunk));
        assert!(Filter::is_in(Field::ChunkId, ["doc_1", "doc_0"]).matches(&chunk));
        assert!(!(!en).matches(&chunk));
    }

    #[test]
    fn validate_rejects_empty_lists_and_unstorable_keys() {
        let rejected = [
            Filter::and([]),
            Filter::or([]),
            Filter::is_in(Field::DocumentId, Vec::<String>::new()),
            Filter::eq(Field::metadata(""), "x"),
            Filter::eq(Field::metadata("a.b"), "x"),
            Filter::eq(Field::metadata("9lives"), "x"),
            Filter::eq(Field::metadata("x".repeat(MAX_FIELD_LEN + 1)), "x"),
            !Filter::and([Filter::exists(Field::metadata("a-b"))]),
        ];
        for filter in rejected {
            assert!(
                matches!(filter.validate(), Err(RagError::UnsupportedFilter(_))),
                "{filter:?} is accepted"
            );
        }
        Filter::eq(Field::metadata("_tenant_2"), "x").validate().expect("valid key");
    }

    #[test]
    fn validate_declared_names_the_undeclared_key() {
        let declared = BTreeSet::from(["lang".to_string()]);
        let filter = Filter::and([
            Filter::eq(Field::metadata("lang"), "en"),
            Filter::eq(Field::metadata("tier"), "gold"),
            Filter::eq(Field::DocumentId, "doc"),
        ]);
        let error = filter.validate_declared("docs", &declared).expect_err("undeclared");
        assert!(error.to_string().contains("`tier` is not declared"), "{error}");
        Filter::eq(Field::SourceUri, "x").validate_declared("docs", &BTreeSet::new()).unwrap();
    }

    #[test]
    fn filters_round_trip_through_json() {
        let filter = Filter::or([
            Filter::eq(Field::metadata("lang"), "en"),
            !Filter::exists(Field::SourceUri),
        ]);
        let json = serde_json::to_value(&filter).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "or": [
                { "eq": [{ "metadata": "lang" }, "en"] },
                { "not": { "exists": "source_uri" } },
            ]})
        );
        assert_eq!(serde_json::from_value::<Filter>(json).unwrap(), filter);
    }
}
