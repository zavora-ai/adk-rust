# Document Readers

Three `DocumentReader` backends read a page into one `DocumentPage`: blocks with their
regions in source pixels, text, confidence, words and table cells, the reading order, and
optionally Markdown. A tool, a graph node or a route that calls `VisionRuntime::read` works
the same whichever backend answers (spec 3.7 scenario S8).

| Backend | Feature | Runs | Answers in | Gives |
|---------|---------|------|------------|-------|
| `PpOcrReader` | `ocr` | On the device: PP-OCRv6 through `oar-ocr` on ONNX Runtime | `SentPixels` | One block per text line, its quad, recognition confidence and words |
| `PaddleOcrVlReader` | `ocr-vl` (`ocr-vl-metal` for Apple GPUs) | On the device: PaddleOCR-VL-1.6 and PP-DocLayoutV3 through `oar-ocr-vl` on Candle | `SentPixels` | Layout blocks (titles, text, tables, formulas, figures, seals) in the layout model's reading order, tables as rows, Markdown |
| `NimDocumentReader` | `nim` | Remote: NVIDIA's CV endpoints | `Normalized { scale: 1, order: Xy }` | Layout blocks from page elements, lines from OCR words, table cells from table structure, chart labels from chart elements |

The local readers implement the trait for `SentImage`; `NimDocumentReader` implements it
for `ReleasedImage` only, so a page reaches NVIDIA only through the egress gate.

## The Page

`DocumentPage` is published as a JSON Schema in
[`adk-vision/schema/document-page.json`](../../../adk-vision/schema/document-page.json); its
regions are `RegionSet` regions, each naming the backend, its coordinate frame and the
transform chain.

| Field | What it holds |
|-------|---------------|
| `blocks[].kind` | `text`, `title`, `table`, `formula`, `figure`, `seal`, `checkbox` or `handwriting` |
| `blocks[].region` | The block's box or polygon in source pixels; `label` is the backend's class (`paragraph`, `doc_title`, `chart`, ...) |
| `blocks[].text` | Lines separated by newlines; a table's rows with cells separated by tabs |
| `blocks[].confidence` | The lowest recognition confidence in the block, else the layout confidence; escalation triggers read it |
| `blocks[].words` | Recognised words with their regions; a chart's words carry their role (`xlabel`, `ylabel`, ...) as the label |
| `blocks[].cells` | Table cells with row, column, spans and text, when the backend measures them |
| `readingOrder` | Indexes into `blocks`, in reading order |
| `markdown` | The page as Markdown, when `ReadOptions::markdown` is set |

Granularity differs by backend; the shape does not:

| Backend | Blocks | Words | Cells |
|---------|--------|-------|-------|
| PP-OCRv6 | One per line | Character boxes merged at spaces | — |
| PaddleOCR-VL-1.6 | Layout blocks | — | — (tables come as rows in `text` and in Markdown; the model gives no cell geometry) |
| NVIDIA NIM | Layout blocks, and one per line outside them | OCR words | From the table-structure model |

## PP-OCRv6

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["ocr", "fetch"] }
```

```rust,ignore
use std::sync::Arc;
use adk_vision::{
    DecodeLimits, FetchConfig, InMemoryAcceptanceStore, ModelFetcher, PpOcrReader, PpOcrV6Size,
    ReadOptions, SourceImage, VisionCx, VisionRuntime,
};

let fetcher = ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
let reader = PpOcrReader::fetch(&fetcher, PpOcrV6Size::Small).await?;
let runtime = VisionRuntime::builder().reader(Arc::new(reader)).build()?;

let page = SourceImage::decode(&std::fs::read("invoice.png")?, &DecodeLimits::default())?;
let read = runtime.read(&VisionCx::new(), &page, &ReadOptions::default()).await?;
for &i in &read.value.reading_order {
    println!("{}", read.value.blocks[i].text);
}
```

The models are PaddlePaddle's own ONNX exports on Hugging Face, byte-identical to the files
`oar-ocr`'s ModelScope registry lists. `oar-ocr`'s `auto-download` stays off: the fetcher
downloads by revision into the Hugging Face cache, checks every file's SHA-256, and the
reader hashes the ONNX bytes again as it loads them. The recognition dictionary is read
from the `inference.yml` pinned beside the model.

| Manifest | Repository | Files | Size |
|----------|------------|-------|------|
| `pp-ocrv6-tiny-det`, `-rec` | `PaddlePaddle/PP-OCRv6_tiny_{det,rec}_onnx` | `inference.onnx` (+ `inference.yml` for rec) | 1.8 MB + 4.5 MB |
| `pp-ocrv6-small-det`, `-rec` | `PaddlePaddle/PP-OCRv6_small_{det,rec}_onnx` | the same | 9.9 MB + 21.2 MB |
| `pp-ocrv6-medium-det`, `-rec` | `PaddlePaddle/PP-OCRv6_medium_{det,rec}_onnx` | the same | 62.0 MB + 76.6 MB |

## PaddleOCR-VL-1.6

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["ocr-vl", "fetch"] }
```

```rust,ignore
use adk_vision::{PaddleOcrVlReader, VlDevice};

let reader = PaddleOcrVlReader::fetch(&fetcher, VlDevice::Cpu).await?;
```

PP-DocLayoutV3 finds the blocks and their order; PaddleOCR-VL-1.6 reads each block's crop
as text, a table, a formula or a chart. Generation runs on a blocking thread, one page at a
time per reader. The loader refuses a model directory that holds a `*.safetensors` file the
manifest does not pin, because Candle would load every one it finds.

| Manifest | Repository | Size |
|----------|------------|------|
| `paddleocr-vl-1.6` | `PaddlePaddle/PaddleOCR-VL-1.6` | 1.93 GB |
| `pp-doclayoutv3` | `PaddlePaddle/PP-DocLayoutV3_safetensors` | 133 MB |

## NVIDIA NIM

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["nim", "ocr", "guardrail", "fetch"] }
```

The page leaves only through the egress gate; here local PP-OCRv6 finds the personal text
(`PersonalTextRedactor`, feature `guardrail`) and the gate fills it before release:

```rust,ignore
use std::sync::Arc;
use adk_core::egress::EgressPolicy;
use adk_vision::{
    CallOptions, EgressAction, EgressConfig, EgressGate, InMemoryEgressRecorder, NimCvConfig,
    NimDocumentReader, PersonalTextRedactor, PpOcrReader, PpOcrV6Size, RedactionClass,
    VisionRuntime,
};

let ocr = PpOcrReader::fetch(&fetcher, PpOcrV6Size::Tiny).await?;
let personal_text = PersonalTextRedactor::new(Arc::new(ocr))
    .with_labelled_fields(["Name", "Address", "Email", "Phone"]);
let reader = NimDocumentReader::new(
    NimCvConfig::new("https://ai.api.nvidia.com/v1/cv").with_api_key_from_env(),
)?;
let gate = EgressGate::new(
    EgressPolicy::allow_private_networks(),
    EgressConfig::default()
        .redacting(RedactionClass::personal_text())
        .with_rule("ai.api.nvidia.com", &["document_page"], EgressAction::Allow),
    Arc::new(InMemoryEgressRecorder::new()),
)
.with_redactor(Arc::new(personal_text));
let runtime = VisionRuntime::builder().remote_reader(Arc::new(reader)).egress_gate(gate).build()?;
let read = runtime
    .read_with(&cx, &page, &ReadOptions::default(), &CallOptions::new().with_data_class("document_page"))
    .await?;
```

| Step | Model | Requests per page |
|------|-------|-------------------|
| Text | `nvidia/nemotron-ocr-v1` (words), `nvidia/nemoretriever-ocr`, or `baidu/paddleocr` (lines) | 1 |
| Blocks | `nvidia/nemotron-page-elements-v3`; elements under confidence 0.3 are ignored | 1 |
| Table cells | `nvidia/nemotron-table-structure-v1` on each table's crop | 1 per table |
| Chart labels | `nvidia/nemotron-graphic-elements-v1` on each chart's crop | 1 per chart |

- **Configuration.** Nothing ships a default URL: name the hosted base URL or a self-hosted
  NIM, and `with_model_url` for a NIM that serves one model at its own URL. The descriptor's
  hosts are those of every URL the reader calls, so the egress policy decides each one.
- **Transport.** Images go inline as PNG data URIs (NVIDIA's asset upload API is retired),
  up to 8 MiB by default. Table and chart crops are cut from the released pixels, so
  nothing outside the released image leaves. The client timeout is 180 s, above the
  gateway's 120 s; a client-side limit allows 40 requests per minute.
- **Cost.** NVIDIA publishes no per-call price, so the descriptor carries none: each read
  records one image and one page at zero cost under the host, and unit caps bound it.
- **Terms.** The hosted endpoints run under NVIDIA's API Trial terms and suit prototypes. A
  self-hosted NIM is free for development and research; serving end users needs NVIDIA AI
  Enterprise.

| Status | Meaning | `AdkError` |
|--------|---------|------------|
| 404 `text/plain` | No such route | `NotFound` `vision.nim.no_route`, naming the URL |
| 404 problem | The key is not entitled to the model | `Forbidden` `vision.nim.not_entitled`; the caller's account id is not repeated |
| 410 | The endpoint is retired | `NotFound` `vision.nim.retired`, with the date |
| 422 | The body was refused | `InvalidInput` |
| 400 | The input was refused | `Unsupported` |
| 429 | Rate limited | `RateLimited`, with `Retry-After` |
| 500, 502, 503 | Worker error, retried once | `Unavailable` |
| 504 | The gateway's 120 s timeout | `Timeout` |

## Measured

On the generated report page (`adk-vision/tests/fixtures/documents/`, 1000×1300, ground truth
from the drawing script), through `VisionRuntime::read` on an Apple Silicon laptop, CPU,
debug build with dependencies at `opt-level = 1`. Character accuracy is over the page's
words; edge error is the largest edge distance between each ground-truth line's ink box and
the backend's line box.

| Backend | Read | Character accuracy | Lines found | Edge error median / max |
|---------|------|--------------------|-------------|-------------------------|
| PP-OCRv6 tiny | 0.1 s | 100.0% | 27/27 | 7 px / 11 px |
| PP-OCRv6 small | 0.4 s | 100.0% | 27/27 | 7 px / 9 px |
| NVIDIA NIM (hosted) | 2.0 s | 99.6% | 27/27 | 1.3 px / 2.1 px |
| PaddleOCR-VL-1.6, CPU | 135.6 s | 100.0% without chart labels, 92.5% with | 4/27 (blocks, not lines) | 5 px / 44 px |
| PaddleOCR-VL-1.6, Metal | 12.4 s | the same | the same | the same |

PP-OCRv6's line boxes include the detector's unclip margin, about 5–7 px around the ink.
PaddleOCR-VL answers in layout blocks, so only single-line blocks match a ground-truth line,
and it reads a chart as a figure: its tick and axis labels need chart recognition, which the
reader does not run.

## Verifying

| Suite | Command |
|-------|---------|
| Contract tests on NIM's recorded answers, and its conformance run | `cargo test -p adk-vision --features nim,conformance --test nim_contract_tests` |
| The page shape across the three backends (S8) | `cargo test -p adk-vision --test s8_document_schema_tests` |
| PP-OCRv6 live, with its conformance run | `cargo test -p adk-vision --features ocr,fetch,conformance --test ocr_live_tests -- --ignored` |
| PaddleOCR-VL-1.6 live, with its conformance run | `cargo test -p adk-vision --features ocr-vl,fetch,conformance --test ocr_vl_live_tests -- --ignored` |
| NIM live through the egress gate, with redaction | `cargo test -p adk-vision --features nim,ocr,guardrail,fetch,conformance --test nim_live_tests -- --ignored` |

## Related

- [Vision overview](index.md)
- [Egress control](../security/egress.md)
