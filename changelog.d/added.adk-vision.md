- **`adk-vision`: local-first perception contracts** (`adk-vision`, new, Experimental; spec 3.7
  §7.1–§7.5, §7.18): artifact-backed `ImageRef`s named by `sha256:` digest, decoded once under
  byte and pixel caps read from the header, with EXIF orientation applied once; `Region`s
  always in source pixels with the `CoordFrame` their backend answered in (sent pixels,
  Gemini's 0–1000 `box_2d`, letterboxed model input) and the `TransformChain` in between,
  `ScreenSpace` for device-pixel-ratio clicks, RLE masks, and `RegionSet` published as a JSON
  Schema; model manifests pinned by commit and SHA-256 with licence classes and recorded
  acceptance, and the `fetch` feature's Hugging Face fetcher that honours `HF_HUB_OFFLINE`
  and refuses a file whose digest differs; the `Detector`, `Segmenter`, `DocumentReader`,
  `Embedder`, `Describer`, `Tracker`, `VideoAnalyzer` and `Editor` traits, generic over
  `SentImage` and `ReleasedImage` so a remote backend only takes images the `EgressGate`
  cropped, redacted, decided and recorded; `VisionRuntime` with local-first routing,
  metering against the run's budget and spend ledger, cancellation, the kill switch and
  crop escalation with `escalatedFrom` provenance; the deterministic `Editor`; and the
  conformance suite with one test macro per trait (`conformance`).
