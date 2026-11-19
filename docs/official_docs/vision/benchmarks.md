# Vision Benchmarks

Latency, accuracy and cost for every `adk-vision` backend, side by side, measured on the
project's own fixtures and published with the machine and versions they ran on. No source
gave a CPU or Apple Silicon latency for the backends `adk-vision` ships, so these tables are
the first; a default backend changes only with a result here.

> **Note:** `adk-vision` is **Experimental** (spec 3.7, §7.17). The tables below are one run on
> one laptop; the nightly job publishes the same table for Apple Silicon and Linux CPU runners
> as an artifact on every run.

## What Is Measured

| Column | How |
|--------|-----|
| Cold load | Fetching the weights from a warm cache, verifying every file's SHA-256 against the built-in manifest, and building the sessions |
| p50 / p95 | Wall time per image through `VisionRuntime` at three input sizes — the fixture scaled to a long edge of 512, 1024 and 2048 px — after warm-up runs; each run changes one pixel, so no embedding cache answers |
| Throughput | Images per second, one at a time, at 1024 px |
| Peak RSS | The process's resident set after the timed runs; the bench process loads backends one after another, so later rows include earlier ones' allocations |
| Scores | Each vision score's mean over the eval set's cases, computed by `adk-eval` exactly as `cargo adk eval` computes it ([Vision Evals](evals.md)) |
| Cost / 1k images | The run's metered spend per 1,000 images: zero for local backends, catalog prices for cloud ones |

The workloads are the committed eval sets over generated images: `generated-detect` for
detectors, `shapes-segment` (box prompts) for promptable segmenters, `shapes-concepts` for
`sam-3.1`, `shapes-locate` and `s5-grounding` for describers, `documents-read` for document
readers, and the four captioned fixtures for embedders (top-1 retrieval image to text and
text to image).

Cloud rows replay recorded provider responses through the real clients: their scores and
cost are what the live call produced, and they carry no latency. A live row is one call,
timed end to end.

## Running It

```bash
# Every local backend plus the recorded cloud rows; weights land in HF_HUB_CACHE
HF_HUB_CACHE=/path/to/models cargo run -p adk-bench --profile ci \
    --features vision-onnx,vision-ocr,vision-cloud --example vision_bench -- \
    run --out vision-bench.md --json vision-bench.json

# A slow backend with fewer runs, added to the same report
HF_HUB_CACHE=/path/to/models cargo run -p adk-bench --profile ci \
    --features vision-ocr-vl-metal --example vision_bench -- \
    run --backends paddleocr-vl-1.6-metal --runs 3 --warmup 1 \
    --append vision-bench.json --out vision-bench.md --json vision-bench.json

# One live call per provider, added to the report
ANTHROPIC_API_KEY=… NVIDIA_API_KEY=… META_API_KEY=… cargo run -p adk-bench --profile ci \
    --features vision-cloud --example vision_bench -- \
    run --backends none --no-recorded --live claude,nvidia,sam \
    --append vision-bench.json --out vision-bench.md --json vision-bench.json
```

`ADK_VISION_ONNX_EP=coreml` (with `vision-coreml`) or `cuda` selects the ONNX execution
provider. The nightly `vision-bench` job in `ci-nightly.yml` runs the first command on
`macos-latest` (Apple Silicon) and `ubuntu-24.04`, uploads `vision-bench-<runner>.md` and
`.json`, and then gates each local backend's eval scores against its committed baseline
through `cargo adk eval --check-regression`.

## Results

<!-- vision-bench:results -->

## Reading the Results

| Backend | What the run shows |
|---------|--------------------|
| RF-DETR Nano | About 52–59 ms per image at every input size on the M4's CPU: the runtime resizes each image to the model's 384×384 input first. Both drawn objects found, region IoU 0.992 |
| RF-DETR Nano q4f16 | A 19 MB export against 108 MB, and on the CPU about 2.5 times slower than fp32 (132–142 ms); it finds both objects, with looser boxes (region IoU 0.890, COCO mAP 0.85 against 1.00) |
| EfficientSAM-Ti, SAM 2.1 Tiny q4 | Each image costs an encoder pass, about 0.6 s and 1.0 s, nearly flat across input sizes. Both segment the drawn shapes at mask IoU 0.98 or better; SAM 2.1's boxes are within 2 px for all three shapes, EfficientSAM's for two |
| SigLIP 2 base | 124–181 ms per image; every caption ranks its own fixture first, and three of four fixtures rank their own caption first |
| PP-OCRv6 tiny, small | Tiny reads a page in 80–113 ms, small in 374–471 ms; both read the report page and the invoice exactly and extract all seven invoice fields |
| Claude Sonnet 5.5, Haiku 5.5 | Every S5 click lands inside its target, at region IoU 0.99; on the drawn shapes, three (Sonnet) and four (Haiku) of five boxes are within 2 px |
| Nemotron Super VL, Gemma 4 | No box lands within 2 px of the drawn edges (region IoU 0.71–0.87); Nemotron places every S5 click inside its target, Gemma 4 all but one. NVIDIA publishes no price for its hosted endpoints, so the rows say unpriced, not free |
| Muse Spark 1.3 | Every S5 click inside its target; boxes looser than Claude's (region IoU 0.81–0.90) |
| `sam-3.1` | Each named shape at region IoU 0.987, $2.50 per 1,000 images as the catalog prices it; one live call took 4.8 s end to end |

The cloud costs are the run's metered spend: a describer's cost follows the tokens the image
becomes, so the 3024×1964 S5 screenshot costs Claude Sonnet 5.5 $16.06 per 1,000 against
$4.67 for the 960×540 shapes image. Local backends record their units at zero cost.

The run shared the machine with other work: the latency rows were measured with the
project's other builds held off, and the load average when the run started is in the
machine table. Load moves the numbers: an earlier run on the same machine, started at a
load average of 48, measured RF-DETR Nano at 76 ms p50 against 52 ms here.

## Related

- [Vision Evals](evals.md)
- [Local ONNX backends](onnx.md)
- [Document readers](document-readers.md)
- [Cloud vision backends](cloud.md)
