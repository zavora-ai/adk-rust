- **Model thinking never reaches an A2A peer** (`adk-server`): the A2A executor and
  `v1::convert::adk_part_to_wire` sent `Part::Thinking` as a text part, so peers read the
  model's reasoning as answer text. `adk_parts_to_a2a` drops thinking parts, and
  `adk_part_to_wire` returns `Result<Option<Part>, A2aError>`, with `None` for a thinking part.
