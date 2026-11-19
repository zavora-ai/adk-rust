- **Cloud vision backends** (`adk-vision`, features `cloud`, `nim`, `sam`; spec 3.7 §7.7,
  §7.9): `LlmDescriber` makes any `adk_core::Llm` that takes images a remote `Describer`
  (or, with `LlmDescriber::local`, an on-device one), with region adapters per model ID —
  Gemini `box_2d` on 0–1000, Claude pixels of the image as sent (the egress gate keeps
  images inside Claude's tier so Claude never resizes), and a prompt-fixed 0–1000 grid for
  NVIDIA Nemotron and Gemma 4 and Muse — and spend holds sized by each provider's
  image-token formula. `NimConfig` builds NVIDIA NIM describers with reasoning off and no
  price. `SamSegmenter` calls Meta `sam-3.1` per concept or object prompt, parses its
  streamed special-token answer into boxes and masks in source pixels, retries rate limits
  with `Retry-After`, and bills one image unit at 2,500 µUSD per request; masks decode
  through a `SamMaskDecoder`, such as `NodeMaskDecoder` over a user-installed
  `@meta-sam/parser`.
- **Per-request concept metering** (`adk-vision`): `BackendDescriptor::concepts_per_request`
  makes the runtime hold and record one request per concept for a backend such as
  `sam-3.1`, so a five-concept call is five image units, held before the first request.
