//! Property tests for memory entry serialization round-trip.
//!
//! **Feature: production-backends, Property 7: Memory Entry Serialization Round-Trip**
//!
//! For any valid `MemoryEntry` with non-empty content, author, and valid timestamp,
//! serializing to JSON and deserializing back produces an equivalent entry.
//!
//! **Validates: Requirements 17.3**

use adk_core::{Content, Part};
use adk_memory::MemoryEntry;
use chrono::{DateTime, TimeZone, Utc};
use proptest::prelude::*;

/// Generate a non-empty author string.
fn arb_author() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9_ -]{0,30}".prop_map(|s| s.trim().to_string())
}

/// Generate a valid role string.
fn arb_role() -> impl Strategy<Value = String> {
    prop_oneof![Just("user".to_string()), Just("model".to_string()), Just("system".to_string()),]
}

/// Generate a non-empty text string for Part::Text.
fn arb_text() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 .,!?]{1,100}"
}

/// Generate a Part::Text variant (the most common for memory entries).
fn arb_text_part() -> impl Strategy<Value = Part> {
    arb_text().prop_map(|text| Part::Text { text })
}

/// Generate a Part::InlineData variant with small data.
fn arb_inline_data_part() -> impl Strategy<Value = Part> {
    (
        prop_oneof![
            Just("image/png".to_string()),
            Just("image/jpeg".to_string()),
            Just("audio/wav".to_string()),
        ],
        proptest::collection::vec(any::<u8>(), 0..64),
    )
        .prop_map(|(mime_type, data)| Part::InlineData { mime_type, data })
}

/// Generate a Part::FileData variant.
fn arb_file_data_part() -> impl Strategy<Value = Part> {
    (
        prop_oneof![Just("image/jpeg".to_string()), Just("application/pdf".to_string()),],
        "https://example\\.com/[a-z]{3,10}\\.[a-z]{3}",
    )
        .prop_map(|(mime_type, file_uri)| Part::FileData { mime_type, file_uri })
}

/// Generate an arbitrary Part that round-trips cleanly through serde.
///
/// We avoid generating `Thinking` parts alongside `Text` parts because
/// `#[serde(untagged)]` deserialization could be ambiguous when both
/// `thinking` and `text` fields are present in different variants.
/// For memory entries, `Text`, `InlineData`, and `FileData` are the
/// realistic variants.
fn arb_part() -> impl Strategy<Value = Part> {
    prop_oneof![arb_text_part(), arb_inline_data_part(), arb_file_data_part(),]
}

/// Generate a non-empty Vec of Parts.
fn arb_parts() -> impl Strategy<Value = Vec<Part>> {
    proptest::collection::vec(arb_part(), 1..5)
}

/// Generate a valid Content.
fn arb_content() -> impl Strategy<Value = Content> {
    (arb_role(), arb_parts()).prop_map(|(role, parts)| Content { role, parts })
}

/// Generate a valid DateTime<Utc> within a reasonable range.
fn arb_timestamp() -> impl Strategy<Value = DateTime<Utc>> {
    // Range: 2020-01-01 to 2030-01-01 (seconds precision for clean round-trip)
    (1_577_836_800i64..1_893_456_000i64).prop_map(|secs| Utc.timestamp_opt(secs, 0).unwrap())
}

/// Generate a valid MemoryEntry.
fn arb_memory_entry() -> impl Strategy<Value = MemoryEntry> {
    (arb_content(), arb_author(), arb_timestamp())
        .prop_map(|(content, author, timestamp)| MemoryEntry { content, author, timestamp })
}

/// Helper: serialize a MemoryEntry to a JSON Value (mirroring PostgresMemoryService).
fn serialize_entry(entry: &MemoryEntry) -> serde_json::Value {
    serde_json::json!({
        "content": serde_json::to_value(&entry.content).unwrap(),
        "author": entry.author,
        "timestamp": entry.timestamp.to_rfc3339(),
    })
}

/// Helper: deserialize a MemoryEntry from a JSON Value (mirroring PostgresMemoryService).
fn deserialize_entry(value: &serde_json::Value) -> MemoryEntry {
    let content: Content = serde_json::from_value(value["content"].clone()).unwrap();
    let author: String = serde_json::from_value(value["author"].clone()).unwrap();
    let timestamp: DateTime<Utc> = serde_json::from_value(value["timestamp"].clone()).unwrap();
    MemoryEntry { content, author, timestamp }
}

/// Compare two Parts for equality.
fn parts_eq(a: &Part, b: &Part) -> bool {
    // Use serde_json round-trip for comparison since Part derives PartialEq
    a == b
}

/// Compare two MemoryEntries for equality.
fn entries_eq(a: &MemoryEntry, b: &MemoryEntry) -> bool {
    a.author == b.author
        && a.timestamp == b.timestamp
        && a.content.role == b.content.role
        && a.content.parts.len() == b.content.parts.len()
        && a.content.parts.iter().zip(b.content.parts.iter()).all(|(pa, pb)| parts_eq(pa, pb))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// **Feature: production-backends, Property 7: Memory Entry Serialization Round-Trip**
    ///
    /// *For any* valid `MemoryEntry` with non-empty content, author, and valid
    /// timestamp, serializing to JSON and deserializing back produces an
    /// equivalent entry.
    ///
    /// **Validates: Requirements 17.3**
    #[test]
    fn prop_memory_entry_json_round_trip(entry in arb_memory_entry()) {
        let json = serialize_entry(&entry);
        let restored = deserialize_entry(&json);
        prop_assert!(entries_eq(&entry, &restored),
            "Round-trip failed.\nOriginal author: {:?}, Restored author: {:?}\n\
             Original role: {:?}, Restored role: {:?}\n\
             Original parts count: {}, Restored parts count: {}",
            entry.author, restored.author,
            entry.content.role, restored.content.role,
            entry.content.parts.len(), restored.content.parts.len()
        );
    }

    /// **Feature: production-backends, Property 7b: Content JSON Round-Trip**
    ///
    /// *For any* valid `Content`, serializing via `serde_json::to_value` and
    /// deserializing via `serde_json::from_value` produces an equivalent Content.
    /// This mirrors the exact serialization path used by `PostgresMemoryService`.
    ///
    /// **Validates: Requirements 17.3**
    #[test]
    fn prop_content_serde_round_trip(content in arb_content()) {
        let json_value = serde_json::to_value(&content).unwrap();
        let restored: Content = serde_json::from_value(json_value).unwrap();

        prop_assert_eq!(&content.role, &restored.role);
        prop_assert_eq!(content.parts.len(), restored.parts.len());
        for (orig, rest) in content.parts.iter().zip(restored.parts.iter()) {
            prop_assert!(parts_eq(orig, rest),
                "Part mismatch: {:?} vs {:?}", orig, rest);
        }
    }

    /// **Feature: production-backends, Property 7c: Timestamp Precision Preservation**
    ///
    /// *For any* valid timestamp, serializing to RFC3339 and parsing back
    /// preserves the value (at second precision).
    ///
    /// **Validates: Requirements 17.3**
    #[test]
    fn prop_timestamp_round_trip(ts in arb_timestamp()) {
        let serialized = ts.to_rfc3339();
        let restored: DateTime<Utc> = serialized.parse().unwrap();
        prop_assert_eq!(ts, restored);
    }
}
