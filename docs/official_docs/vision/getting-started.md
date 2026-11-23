# Getting Started: a Document Desk

A Document desk reads scanned invoices, receipts, contracts and identity cards into tables on
your computer, and sends an image to a cloud model only when it cannot read a field — and
then only the field's crop, redacted, after you approve that exact image. This page builds
one from the `vision` template and walks through what each part does.

> **Note:** `adk-vision` is **Experimental** (spec 3.7). The desk runs PP-OCRv6 and RF-DETR
> on ONNX Runtime, which `vision-local` downloads at build time.

## 1. Scaffold the Desk

```bash
cargo install cargo-adk
cargo adk new claims-desk --template vision
cd claims-desk
cp .env.example .env        # ANTHROPIC_API_KEY for the cloud model
```

The project depends on `adk-rust` alone:

```toml
[dependencies]
adk-rust = { version = "3.0.0", default-features = false, features = ["minimal", "harness", "vision-local", "artifacts", "cli", "anthropic"] }
```

| Feature | What it brings |
|---------|----------------|
| `vision` (implied by `vision-local`) | `adk_rust::vision`: regions, manifests and the fetcher, the egress gate, the cloud describers, the image privacy guardrail; with `harness`, the vision pack |
| `vision-local` | The on-device backends: PP-OCRv6, PaddleOCR-VL-1.6, RF-DETR, EfficientSAM, SAM 2.1 and SigLIP 2 |
| `harness`, `cli` | The governed harness agent, and the terminal prompter that asks you before an image leaves |

| File | Holds |
|------|-------|
| `src/main.rs` | The model, the harness with the vision pack, and the loop over `inbox/` |
| `src/desk.rs` | The on-device models, the vision pack and the harness over an artifact store |
| `.adk/vision.toml` | What may leave the machine, always after redaction |
| `tests/desk_smoke.rs` | A cloud describe held for a person, refused, with nothing sent |

## 2. Run It

```bash
cp ~/scans/*.png inbox/
cargo run
```

The first run fetches PP-OCRv6 tiny (6 MB) and RF-DETR Nano (108 MB) by revision into the
Hugging Face cache and checks every file's SHA-256 before ONNX Runtime loads it. Set
`HF_HUB_CACHE` to keep the cache elsewhere, and `HF_HUB_OFFLINE=1` once it is warm: the
fetcher then opens no socket.

For each page the agent calls `vision_read`, which runs PP-OCRv6 on your computer, and writes
one row per page to `tables/<kind>.csv`. Nothing about these calls leaves the machine.

## 3. What Runs Where

| Step | Backend | Where |
|------|---------|-------|
| Read a page | `pp-ocrv6-tiny` (`PpOcrReader`) | This computer |
| Find people to cover | `rf-detr-n` (`RfDetr`) behind `PersonRedactor`, or `yunet-2023mar` (`YuNet`) for faces behind `FaceRedactor` | This computer |
| Find personal text to cover | PP-OCRv6 and the PII patterns behind `PersonalTextRedactor` | This computer |
| Describe a region the reader could not read | The cloud model (`LlmDescriber::remote`) | The provider, after the egress gate |

`desk::pack` wires them into the vision pack:

```rust,ignore
VisionPack::new(MediaStore::new(artifacts), move |gate, store| {
    VisionRuntime::builder()
        .reader(reader)                 // on the device
        .remote_describer(describer)    // only released images
        .shared_egress_gate(gate)
        .media_store(store)
        .build()
})
.redactor(Arc::new(PersonRedactor::new(people)))
.redactor(Arc::new(PersonalTextRedactor::new(reader).with_labelled_fields(["Name"])))
```

A remote backend takes only a `ReleasedImage`, which only the egress gate constructs, so the
describer cannot be handed a page the gate has not cropped, redacted and recorded.

## 4. What May Leave

`.adk/vision.toml` holds the rules:

```toml
[egress]
default = "ask"
redact = ["faces", "personal_text"]

[[egress.rule]]
host = "api.anthropic.com"
classes = ["document"]
action = "deny"

[[egress.rule]]
host = "api.anthropic.com"
classes = ["document_crop"]
action = "ask"
```

| Rule | Effect |
|------|--------|
| `redact` | Faces (the face, or a detected person's whole box when no face is found; spec 3.7 Q3) and personal text are filled solid before any decision; the pack adds both even if the file leaves them out |
| `document` → `deny` | A whole page never leaves |
| `document_crop` → `ask` | A field's crop waits for you: the terminal shows the host, the data class, what was covered and the image's digest, and only that exact image is released |

A rule that says `allow` releases without asking; in the harness, an `allow` in the
workspace's own file counts only when the operator trusts it
(`VisionPack::trust_workspace_rules`). See [Governed Vision Tools](governed-tools.md) and
[Image Privacy](../security/image-privacy.md).

## 5. Check What Left

Every release and refusal is an egress record: source and egress digests, destination, data
class, redactions by class and the decision. With the CLI installed with `vision`, the
project's log is one command away:

```bash
cargo install adk-cli --features vision-local
adk-rust vision egress log
```

[The `adk-rust vision` CLI](cli.md) also lists and fetches models, records licence
acceptance and runs one perception call on an image.

## 6. Test It

```bash
cargo test
```

`tests/desk_smoke.rs` plays the agent and the provider with a scripted model: the agent asks
to describe an identity card, the call is held for a person, the person says no, and the
provider receives nothing. It needs no key and no model download.

## The Scenarios Behind It

`examples/vision` (package `vision-scenarios`) runs spec 3.7's scenarios S1–S13. S1–S8, the
Document desk, run on forty generated pages with invented data: PP-OCRv6 and RF-DETR on the
device, NVIDIA NIM, Claude, Gemini and Meta `sam-3.1` on recorded responses through their real
clients. S9–S13 — the Inspector on a conveyor stream, visual wakes, frame budgets, the live
camera surface and native video — are in [The vision scenarios](scenarios.md).

```bash
cargo run --manifest-path examples/vision/Cargo.toml
cargo run --manifest-path examples/vision/Cargo.toml -- s3_redact_before_it_leaves
```

| Scenario | What passes |
|----------|-------------|
| S1 Document desk, local first | Forty pages read on the device into one table per kind; each value carries page, region and backend; pages whose blocks all clear the threshold send nothing |
| S2 Escalate the crop, not the page | Exactly five crops leave, one per hard field, each its block plus margin |
| S3 Redact before it leaves | The bytes the provider received hold no face and none of the card's personal strings |
| S4 Approve with the image | The held call carries the exact egress image; approval releases that digest; a swapped image is held again |
| S5 One region, four frames | Claude, Gemini, `sam-3.1` and RF-DETR answers for twelve targets land within 2 px |
| S6 Weights you can trust | Offline fetch opens no socket; a tampered file is refused; a restricted model waits for acceptance |
| S7 A budget per image | A run capped at 20 images makes 20 `sam-3.1` calls of 30; the rest are refused before any request |
| S8 Same tool, any backend | One tool call shape across PP-OCRv6, PaddleOCR-VL-1.6 and NIM, and RF-DETR and `sam-3.1` |

## Related

- [The vision scenarios](scenarios.md)
- [Vision overview](index.md)
- [The `adk-rust vision` CLI](cli.md)
- [Local ONNX backends](onnx.md)
- [Document readers](document-readers.md)
- [Cloud backends](cloud.md)
