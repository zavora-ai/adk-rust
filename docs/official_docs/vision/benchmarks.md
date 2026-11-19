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

<!-- vision-bench:reading -->

## Related

- [Vision Evals](evals.md)
- [Local ONNX backends](onnx.md)
- [Document readers](document-readers.md)
- [Cloud vision backends](cloud.md)
