# vision

Scenarios S1–S8, S18 and S19 of spec 3.7 §5 for `adk-vision`: the Document desk and the rest
of phase A, face blur on streams and the on-device profile. Every pass condition is asserted
on egress records, audit records, approvals, the spend ledger, the store and the pixels that
left, never on model prose.

| Runs | How |
|------|-----|
| On the device | PP-OCRv6 tiny reads, RF-DETR Nano detects people and YuNet faces, fetched by revision through the model fetcher and checked by SHA-256 on the first run |
| In a sandbox | S19 runs this binary again under Seatbelt (macOS) or bubblewrap (Linux, `bwrap` installed and unprivileged user namespaces allowed) with the network denied |
| In the cloud | NVIDIA NIM, Claude, Gemini and Meta `sam-3.1` answer from recorded responses (`fixtures/recorded/`), served to their real clients from a local server, so no key is needed |
| The agent | A harness with the vision pack on a scripted planner |

```bash
cargo run                                   # every scenario, then a pass table
cargo run -- s3_redact_before_it_leaves     # one scenario
cargo run -- --report vision-scenarios.md   # the table as Markdown
```

The first run downloads about 115 MB of weights into the Hugging Face cache (YuNet is 232 KB of it); set
`HF_HUB_CACHE` to keep them elsewhere. CI caches them.

| Scenario | What passes | Runs on |
|----------|-------------|---------|
| `s1_document_desk_local_first` | A harness reads the forty pages through `vision_read` into one table per document kind; each value carries its page digest, region in source pixels and backend; PP-OCRv6's field accuracy is at or above its recorded baseline; the 35 pages whose blocks all clear 0.8 send zero bytes off the machine; each on-device read is a `vision.call` record with its image units at zero cost; vision spend equals the sum of the ledger entries | PP-OCRv6 tiny on the device |
| `s2_escalate_the_crop_not_the_page` | The three handwritten amounts and two stamped values each send exactly one crop, their block plus 8 px, never the page; where NIM read text, its answer replaces the local block with `escalatedFrom`; each egress record names host, source and egress digests and redactions, and keeps the egress copy; NIM is unpriced, so nothing is estimated or charged | NIM `nemotron-ocr-v1`, recorded |
| `s3_redact_before_it_leaves` | RF-DETR finds the card's generated portrait and PP-OCRv6 reads its four personal strings; in the bytes Claude received, RF-DETR finds no face and PP-OCRv6 none of the strings; the egress copy is an artifact with the egress digest; the `vision.egress.released` record links both digests and counts redactions by class | Claude Haiku 5.5, recorded |
| `s4_approve_with_the_image` | In Ask mode the call is held with the exact egress image, its regions and digest; approval releases that digest and the provider receives exactly it; a swapped image changes the digest and fingerprint and is held again; the audit records link each decision to its digest | Claude Sonnet 5.5, recorded |
| `s5_one_region_four_frames` | Claude (pixels it processed), Gemini (`box_2d` on 0–1000), `sam-3.1` (source pixels) and RF-DETR (letterboxed model input) answers for twelve targets: all 48 within 2 px; every click point at device pixel ratio 2 inside its target; 1,000 random transform chains round-trip within 0.5 px | Claude and `sam-3.1` recorded live; Gemini and RF-DETR synthetic, in their documented frames |
| `s6_weights_you_can_trust` | The desk's models fetched by commit revision; with `HF_HUB_OFFLINE=1` and a warm cache no socket opens; a tampered file is refused at fetch naming both digests, and one changed after verification is refused before ONNX Runtime opens it; a use-restricted model needs a recorded acceptance naming its licence URL; the built-in manifest lists no non-commercial or copyleft model | The real fetcher, a local fixture hub |
| `s7_a_budget_per_image` | A run capped at 20 images makes 30 `sam-3.1` calls: 20 run, the rest are refused before any request naming the unit and cap; the ledger holds 20 commits of 2,500 µUSD; an on-device call records its image at zero cost | `sam-3.1` recorded live |
| `s8_same_tool_any_backend` | One `vision_read` shape across PP-OCRv6, PaddleOCR-VL-1.6 and NIM, one `vision_detect` shape across RF-DETR and `sam-3.1`; each page validates against `schema/document-page.json`; accuracy, latency and cost side by side; PP-OCRv6, NIM and RF-DETR pass the conformance suite | PaddleOCR-VL-1.6 replayed from its recorded page (1.9 GB of weights); its and `sam-3.1`'s conformance run in adk-vision's own tests |
| `s18_blur_by_default_on_streams` | Two drawn passers-by cross the conveyor camera, one close and one far, for 70 frames, every one kept: every face is blurred before any other stage; YuNet from 0.3 finds no face in any stored frame; each image written is a frame's redacted pixels, written after its blur; no frame that showed a face reaches the store as captured; the close passer-by's blur covers under half the figure and the shirt is stored as captured; frames with nobody are stored as captured; on a close desk frame the image privacy guardrail covers the face, not the frame | YuNet and RF-DETR Nano on the device |
| `s19_off_the_network` | S1 again inside the operating system's sandbox with `HF_HUB_OFFLINE=1`, a warm cache and `on_device = true`: a direct connection from inside is refused; the forty pages are read on the device; each of the five escalations is refused at the egress gate's destination step with its reason and nothing is released; run again behind an egress proxy that admits no host, the sandbox egress record is empty | PP-OCRv6 tiny on the device, Seatbelt or bubblewrap |

## Fixtures

| Path | What it is |
|------|------------|
| `fixtures/documents/` | Twelve invoices, twelve receipts, six contracts and ten identity cards drawn by `generate.py`; every personal detail is invented and every portrait is a drawn shape captioned as generated. `documents.json` is the ground truth |
| `fixtures/recorded/` | The live responses the cloud scenarios replay, and the proxy that recorded them |
| `fixtures/baselines.json` | The field accuracy each reader reached when the scenarios were accepted |
| `../../adk-vision/tests/fixtures/` | The S5 screen, the report page and adk-vision's own recordings |
