//! Size limits for the tool schemas an MCP server publishes.

use serde_json::{Map, Value};

/// Size limits applied to each tool schema an MCP server publishes.
///
/// [`McpToolset`](super::McpToolset) measures a tool's input schema and, when
/// present, its output schema during discovery, before it copies or logs either
/// document. A tool whose schema exceeds a limit is skipped with a `warn!` naming
/// the toolset and the tool, and discovery continues with the remaining tools.
/// Accepted tools log the measured size at `debug`, never the document.
///
/// | Limit | Measures | Default |
/// |-------|----------|---------|
/// | `max_bytes` | Approximate compact-JSON size of one schema | 262 144 (256 KiB) |
/// | `max_nodes` | JSON values in one schema, the root object included | 10 000 |
///
/// The byte measure counts each key and string by its decoded length plus two
/// quotes, each number by its display length, and every `{}`, `[]`, `:` and `,`.
/// For a document without escape sequences it equals the compact serialization
/// length. The walk stops at the first value that takes the schema over a limit,
/// so the size a warning reports is the size measured up to that value.
///
/// # Defaults
///
/// - **256 KiB** is about 64 000 tokens for one tool. A model request carries
///   every tool schema, so a larger schema leaves too little of the context window
///   to be usable, while published MCP tool schemas run from hundreds of bytes to
///   tens of kilobytes.
/// - **10 000 nodes** is what a typical schema, at roughly 25 bytes per value,
///   holds at the byte limit. It bounds documents of many small values, such as
///   `[0,0,0]`, whose in-memory cost per value far exceeds their serialized size.
///
/// # Example
///
/// ```
/// use adk_tool::mcp::McpSchemaLimits;
///
/// let limits = McpSchemaLimits::default().with_max_bytes(64 * 1024).with_max_nodes(2_000);
///
/// assert_eq!(limits.max_bytes, 65_536);
/// assert_eq!(limits.max_nodes, 2_000);
/// assert_eq!(McpSchemaLimits::default().max_bytes, McpSchemaLimits::DEFAULT_MAX_BYTES);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct McpSchemaLimits {
    /// Largest accepted schema, in bytes as measured above.
    pub max_bytes: usize,
    /// Most JSON values accepted in one schema.
    pub max_nodes: usize,
}

impl McpSchemaLimits {
    /// Default value of [`max_bytes`](Self::max_bytes): 256 KiB.
    pub const DEFAULT_MAX_BYTES: usize = 256 * 1024;

    /// Default value of [`max_nodes`](Self::max_nodes).
    pub const DEFAULT_MAX_NODES: usize = 10_000;

    /// Sets the largest accepted schema size in bytes.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_tool::mcp::McpSchemaLimits;
    ///
    /// let limits = McpSchemaLimits::default().with_max_bytes(1024 * 1024);
    /// assert_eq!(limits.max_bytes, 1_048_576);
    /// ```
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Sets the most JSON values accepted in one schema.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_tool::mcp::McpSchemaLimits;
    ///
    /// let limits = McpSchemaLimits::default().with_max_nodes(50_000);
    /// assert_eq!(limits.max_nodes, 50_000);
    /// ```
    pub fn with_max_nodes(mut self, max_nodes: usize) -> Self {
        self.max_nodes = max_nodes;
        self
    }

    /// Measures `schema`, stopping at the first value that takes it over a limit.
    ///
    /// Returns the full size when the schema is within both limits. Otherwise
    /// returns the size measured when the walk stopped, which exceeds at least one
    /// limit.
    pub(crate) fn measure(
        &self,
        schema: &Map<String, Value>,
    ) -> std::result::Result<SchemaSize, SchemaSize> {
        // One iterator per open container, so nesting depth costs heap, not stack.
        enum Members<'a> {
            Object(serde_json::map::Iter<'a>),
            Array(std::slice::Iter<'a, Value>),
        }
        // Brackets plus the commas between `len` members.
        let container_bytes = |len: usize| 2 + len.saturating_sub(1);

        let mut size = SchemaSize { bytes: container_bytes(schema.len()), nodes: 1 };
        let mut open = vec![Members::Object(schema.iter())];
        while size.bytes <= self.max_bytes && size.nodes <= self.max_nodes {
            let next = match open.last_mut() {
                None => return Ok(size),
                Some(Members::Object(entries)) => {
                    entries.next().map(|(key, value)| (Some(key), value))
                }
                Some(Members::Array(items)) => items.next().map(|value| (None, value)),
            };
            let Some((key, value)) = next else {
                open.pop();
                continue;
            };
            size.nodes += 1;
            if let Some(key) = key {
                // The key's quotes and the colon after it.
                size.bytes += key.len() + 3;
            }
            size.bytes += match value {
                Value::Null | Value::Bool(true) => 4,
                Value::Bool(false) => 5,
                Value::Number(number) => number.to_string().len(),
                Value::String(text) => text.len() + 2,
                Value::Array(items) => {
                    open.push(Members::Array(items.iter()));
                    container_bytes(items.len())
                }
                Value::Object(entries) => {
                    open.push(Members::Object(entries.iter()));
                    container_bytes(entries.len())
                }
            };
        }
        Err(size)
    }
}

impl Default for McpSchemaLimits {
    fn default() -> Self {
        Self { max_bytes: Self::DEFAULT_MAX_BYTES, max_nodes: Self::DEFAULT_MAX_NODES }
    }
}

/// Size of one schema as [`McpSchemaLimits::measure`] counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SchemaSize {
    /// Approximate compact-JSON size in bytes.
    pub(crate) bytes: usize,
    /// JSON values, the root object included.
    pub(crate) nodes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("expected a JSON object, got {other}"),
        }
    }

    fn unlimited() -> McpSchemaLimits {
        McpSchemaLimits { max_bytes: usize::MAX, max_nodes: usize::MAX }
    }

    /// The input schema of a typical issue-tracker tool.
    fn realistic_schema() -> Map<String, Value> {
        object(json!({
            "type": "object",
            "properties": {
                "repository": { "type": "string", "description": "Repository as owner/name." },
                "title": { "type": "string", "minLength": 1, "maxLength": 256 },
                "labels": { "type": "array", "items": { "type": "string" }, "maxItems": 20 },
                "priority": { "type": "string", "enum": ["low", "normal", "high"] },
                "weight": { "type": "number", "minimum": 0.5 },
                "draft": { "type": "boolean", "default": false },
                "assignee": { "type": ["string", "null"], "default": null }
            },
            "required": ["repository", "title"],
            "additionalProperties": false
        }))
    }

    #[test]
    fn defaults_are_256_kib_and_10_000_nodes() {
        assert_eq!(
            McpSchemaLimits::default(),
            McpSchemaLimits { max_bytes: 262_144, max_nodes: 10_000 }
        );
    }

    #[test]
    fn the_byte_measure_equals_compact_serialization_without_escapes() {
        let schema = realistic_schema();
        let compact = serde_json::to_string(&schema).unwrap().len();

        // The root, 4 top-level values, 7 property objects, 22 values inside those
        // objects, and the 2 required names.
        let expected = SchemaSize { bytes: compact, nodes: 36 };
        assert_eq!(unlimited().measure(&schema), Ok(expected));
    }

    #[test]
    fn the_default_limits_accept_a_realistic_schema() {
        let schema = realistic_schema();
        assert_eq!(McpSchemaLimits::default().measure(&schema), unlimited().measure(&schema));
    }

    #[test]
    fn a_schema_exactly_at_the_byte_limit_is_accepted_and_one_byte_over_is_rejected() {
        let schema = realistic_schema();
        let full = SchemaSize { bytes: serde_json::to_string(&schema).unwrap().len(), nodes: 36 };

        let at_limit = unlimited().with_max_bytes(full.bytes);
        assert_eq!(at_limit.measure(&schema), Ok(full));

        // The final value is the one that crosses the limit, so the walk measured everything.
        let below = unlimited().with_max_bytes(full.bytes - 1);
        assert_eq!(below.measure(&schema), Err(full));
    }

    #[test]
    fn a_schema_exactly_at_the_node_limit_is_accepted_and_one_node_over_is_rejected() {
        let schema = realistic_schema();
        let full = SchemaSize { bytes: serde_json::to_string(&schema).unwrap().len(), nodes: 36 };

        assert_eq!(unlimited().with_max_nodes(36).measure(&schema), Ok(full));
        assert_eq!(unlimited().with_max_nodes(35).measure(&schema), Err(full));
    }

    #[test]
    fn a_wide_shallow_schema_stops_at_the_node_limit() {
        let properties: Map<String, Value> =
            (0..5_000).map(|index| (format!("p{index}"), json!({ "type": "string" }))).collect();
        let schema = object(json!({ "type": "object", "properties": properties }));
        let full = SchemaSize {
            bytes: serde_json::to_string(&schema).unwrap().len(),
            // Root, "type", "properties", then an object and a string per property.
            nodes: 3 + 5_000 * 2,
        };
        assert!(full.bytes < McpSchemaLimits::DEFAULT_MAX_BYTES, "only the node limit applies");

        let stopped = McpSchemaLimits::default().measure(&schema).unwrap_err();
        assert_eq!(stopped.nodes, McpSchemaLimits::DEFAULT_MAX_NODES + 1);
        assert!(stopped.bytes < full.bytes, "the walk stops before the last property");

        let raised = McpSchemaLimits::default().with_max_nodes(full.nodes);
        assert_eq!(raised.measure(&schema), Ok(full));
    }

    #[test]
    fn a_single_long_string_exceeds_the_byte_limit_on_its_own() {
        let description = "x".repeat(1024 * 1024);
        let schema = object(json!({ "description": description, "type": "string" }));

        // Brackets and one comma, `"description":`, then the quoted string. The walk
        // stops there, before "type", in either map ordering.
        let stopped = SchemaSize { bytes: 3 + 14 + (1024 * 1024 + 2), nodes: 2 };
        assert_eq!(McpSchemaLimits::default().measure(&schema), Err(stopped));
    }

    #[test]
    fn a_long_key_counts_toward_the_byte_limit() {
        let schema = Map::from_iter([("k".repeat(300 * 1024), Value::Bool(true))]);

        let stopped = SchemaSize { bytes: 2 + (300 * 1024 + 3) + 4, nodes: 2 };
        assert_eq!(McpSchemaLimits::default().measure(&schema), Err(stopped));
    }

    #[test]
    fn the_root_object_counts_as_a_node() {
        let empty = Map::new();
        assert_eq!(unlimited().measure(&empty), Ok(SchemaSize { bytes: 2, nodes: 1 }));
        assert_eq!(
            unlimited().with_max_nodes(0).measure(&empty),
            Err(SchemaSize { bytes: 2, nodes: 1 })
        );
    }
}
