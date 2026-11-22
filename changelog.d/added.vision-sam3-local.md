- **SAM 3 on the device from your own weights** (`adk-vision`; spec 3.7 §7.5, Q4): the opt-in
  `sam3-local` feature adds `Sam3`, a `Segmenter` and `Detector` for concept prompts (masks
  when segmenting, boxes when detecting, scored as Meta's `Sam3Processor` scores them), and
  `Sam3Tracker` for point and box prompts, on ONNX Runtime. Meta's SAM 3 weights are behind an
  access request and under the SAM License, so no weights and no built-in record ship: you
  export the graphs, write a manifest naming `LicenseRef-SAM` as `useRestricted`, and record
  acceptance with `adk-rust vision models accept <id>`; until then verification refuses with
  `licence acceptance required` and the licence URL, and a manifest that classes SAM 3 weights
  otherwise is refused. In no `adk-rust` tier.
