- **Gemini grounding keeps search suggestions and part indices** (`adk-gemini`):
  `GroundingMetadata` gains `search_entry_point: Option<SearchEntryPoint>` (the new
  `SearchEntryPoint` holds `rendered_content` and `sdk_blob`) and `GroundingSegment` gains
  `part_index: Option<u32>`, so `searchEntryPoint` and `segment.partIndex` survive parsing and
  serialization and reach the `LlmResponse::provider_metadata` that `GeminiModel` fills from
  grounding metadata. Struct literals of either type set the new field or end with
  `..Default::default()`; `GroundingMetadata` now derives `Default`. The API omits a zero
  `partIndex`, so `None` means the first part.
