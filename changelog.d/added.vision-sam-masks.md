- **`sam-3.1` masks: a licensed codec or your own decoder** (`adk-vision`,
  `adk-vision-sam-codec`, `adk-cli`; spec 3.7 §7.7): `MaskDecoderChoice` picks how Meta's
  mask payloads become masks — `boxes` (the default, each object its box), `external` (a
  program you installed, reading mask JSON on standard input and writing base64 rasters, with
  a timeout and a clear error for a missing program or malformed output), or `licensed`.
  `licensed` decodes in process with the new `adk-vision-sam-codec` crate, a Rust port of the
  mask codec in Meta's `@meta-sam/parser` 0.1.0 that matches Meta's rasters bit for bit and is
  distributed under the SAM License rather than Apache-2.0; it is reached only through the
  opt-in `sam-codec` feature (CLI `vision-sam-codec`), in no `adk-rust` tier, and refuses with
  `licence acceptance required` until a person runs `adk-rust vision models accept
  sam-mask-codec` (a built-in manifest entry with the new `RuntimeSpec::Native`).
  `SamConfig::with_mask_decoder`, the `[sam.masks]` table of `.adk/vision.toml` and
  `adk-rust vision run --mask-decoder` choose it; `ExternalMaskDecoder::meta_parser` runs
  Meta's parser in a user-installed Node.js.
