- **Gemini grounding retains search suggestions and part indices** (`adk-gemini`):
  `GroundingMetadata` gains `search_entry_point: Option<SearchEntryPoint>` and
  `GroundingSegment` gains `part_index: Option<u32>`, so `searchEntryPoint` and
  `segment.partIndex` survive response parsing and serialization. Struct literals of
  either type must set the new field or end with `..Default::default()`;
  `GroundingMetadata` and `SearchEntryPoint` now derive `Default`. The API omits a zero
  `partIndex`, so `None` means the first part.
