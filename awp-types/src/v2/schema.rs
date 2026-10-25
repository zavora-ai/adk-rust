//! JSON Schemas (2020-12) for the AWP 2.0 wire documents (spec 3.5 §7.2–§7.8.3).
//!
//! Requires the `json-schema` feature. The committed copies live in
//! `awp-types/schemas/2.0/`, and a test fails when they drift from these types.

use schemars::generate::SchemaSettings;
use schemars::transform::{Transform, transform_subschemas};
use schemars::{JsonSchema, Schema};

use super::{
    CapabilityManifestV2, CapabilityV2, CatalogChangesPage, Charter, ConsentReceipt, ErrorEnvelope,
    EventEnvelope, EventSubscription, HeldCall, KeySet, LogEntry, MachineView, PublicTerms,
    QuoteDocument, Receipt, ReleaseManifest, SiteContract, SiteStatus, StreamEntry, TrustRules,
};

/// Base URL of the published schemas; each schema's `$id` is `<base><name>.schema.json`.
pub const SCHEMA_BASE: &str = "https://agenticwebprotocol.com/schemas/2.0/";

/// Returns the schema of every AWP 2.0 wire document, by file stem, each with its `$id`.
///
/// | Stem | Document |
/// |---|---|
/// | `site-contract` | `/.well-known/awp.json` |
/// | `key-set` | `keys.json` |
/// | `trust-rules` | the contract's `identity.trustRules` |
/// | `capability` | one capability |
/// | `capability-manifest` | `GET /awp/v2/manifest` |
/// | `machine-view` | `<path>/index.awp.json` |
/// | `held-call` | `GET /awp/v2/calls/{id}` |
/// | `event-envelope` | a webhook event |
/// | `subscription` | `POST`/`GET /awp/v2/events/{id}` |
/// | `catalog-changes-page` | a `catalog.changed` event's `next` page |
/// | `site-status` | `GET /awp/v2/status` |
/// | `consent-receipt` | a consent receipt |
/// | `quote` | a quote |
/// | `receipt` | a commerce receipt |
/// | `error-envelope` | an error response |
/// | `release-manifest` | `/.well-known/awp/releases/{seq}.json` |
/// | `log-entry` | a site log leaf |
/// | `stream-entry` | a signed stream entry |
/// | `charter` | the full charter, kept in the charter store |
/// | `charter-public-terms` | `/.well-known/awp/charter.json` |
///
/// # Example
///
/// ```
/// let schemas = awp_types::v2::schema::wire_schemas();
/// let (_, contract) = schemas.iter().find(|(stem, _)| *stem == "site-contract").unwrap();
/// assert_eq!(
///     contract.get("$id").and_then(|id| id.as_str()),
///     Some("https://agenticwebprotocol.com/schemas/2.0/site-contract.schema.json")
/// );
/// ```
pub fn wire_schemas() -> Vec<(&'static str, Schema)> {
    vec![
        with_id::<SiteContract>("site-contract"),
        with_id::<KeySet>("key-set"),
        with_id::<TrustRules>("trust-rules"),
        with_id::<CapabilityV2>("capability"),
        with_id::<CapabilityManifestV2>("capability-manifest"),
        with_id::<MachineView>("machine-view"),
        with_id::<HeldCall>("held-call"),
        with_id::<EventEnvelope>("event-envelope"),
        with_id::<EventSubscription>("subscription"),
        with_id::<CatalogChangesPage>("catalog-changes-page"),
        with_id::<SiteStatus>("site-status"),
        with_id::<ConsentReceipt>("consent-receipt"),
        with_id::<QuoteDocument>("quote"),
        with_id::<Receipt>("receipt"),
        with_id::<ErrorEnvelope>("error-envelope"),
        with_id::<ReleaseManifest>("release-manifest"),
        with_id::<LogEntry>("log-entry"),
        with_id::<StreamEntry>("stream-entry"),
        with_id::<Charter>("charter"),
        with_id::<PublicTerms>("charter-public-terms"),
    ]
}

fn with_id<T: JsonSchema>(stem: &'static str) -> (&'static str, Schema) {
    let mut schema = SchemaSettings::draft2020_12()
        .with_transform(TrimExamples)
        .into_generator()
        .into_root_schema_for::<T>();
    schema.insert("$id".to_owned(), format!("{SCHEMA_BASE}{stem}.schema.json").into());
    (stem, schema)
}

/// Turns rustdoc into plain schema descriptions: drops the `# Example` sections and the link
/// targets of intra-doc links, which mean nothing outside Rust.
#[derive(Clone)]
struct TrimExamples;

impl Transform for TrimExamples {
    fn transform(&mut self, schema: &mut Schema) {
        if let Some(serde_json::Value::String(description)) = schema.get_mut("description") {
            if let Some(at) = description.find("\n\n# Example") {
                description.truncate(at);
            }
            *description = strip_doc_links(description);
        }
        transform_subschemas(self, schema);
    }
}

/// Rewrites `` [`Name`](path) `` and `` [`Name`] `` to `` `Name` ``; keeps links to URLs.
fn strip_doc_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            out.push('[');
            rest = after;
            continue;
        };
        let (label, tail) = (&after[..close], &after[close + 1..]);
        if let Some(target) = tail.strip_prefix('(')
            && let Some(end) = target.find(')')
            && !target[..end].contains("://")
        {
            out.push_str(label);
            rest = &target[end + 1..];
        } else if tail.starts_with('(') {
            out.push('[');
            rest = after;
        } else if label.starts_with('`') && label.ends_with('`') {
            out.push_str(label);
            rest = tail;
        } else {
            out.push('[');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::strip_doc_links;

    #[test]
    fn doc_links_become_code_spans() {
        assert_eq!(
            strip_doc_links("URL of the published [`PublicTerms`](super::charter::PublicTerms)."),
            "URL of the published `PublicTerms`."
        );
        assert_eq!(strip_doc_links("data is [`CatalogChanged`]."), "data is `CatalogChanged`.");
        assert_eq!(
            strip_doc_links("see [the spec](https://example.com/spec) and [x, y]"),
            "see [the spec](https://example.com/spec) and [x, y]"
        );
    }
}
