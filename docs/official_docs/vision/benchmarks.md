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

Measured on 2026-11-19 with the commands above, built with the `ci` profile. The local rows
ran first, from a warm weights cache; the live calls, PaddleOCR-VL-1.6 on Metal (2 timed runs
per size) and on the CPU (one timed run at 1024 px) ran later and were appended, each noting the
load average it started under.
Recorded rows replay the responses recorded on 2026-11-19.

> **Note:** These rows ran on `ort` 2.0.0-rc.13 and onnxruntime 1.28. On 2026-11-21 the
> `rf-detr-n`, `rf-detr-n-q4f16`, `pp-ocrv6-tiny` and `pp-ocrv6-small` rows ran again on `ort`
> 2.0.0-rc.15 and onnxruntime 1.31, alternating with the earlier build on the same machine: the
> same scores (`rf-detr-n-q4f16` region IoU 0.892 against 0.890), latency within run-to-run
> noise, and a lower peak RSS.

| Machine | |
|---|---|
| Run | 2026-11-19T17:27:18Z |
| OS | macOS 26.2 (aarch64) |
| CPU | Apple M4, 10 logical cores |
| Memory | 24 GiB |
| Load average at start | 9.61 13.24 16.05 |
| adk-rust | 3.0.0 |
| image | 0.25.10 |
| oar-ocr | 0.10.3 |
| oar-ocr-vl | 0.10.3 |
| onnxruntime | 1.28.0 |
| ort | 2.0.0-rc.13 |
| profile | optimised |
| tokenizers | 0.21.4 |
| Settings | 2 warm-up and 10 timed runs per size; sizes are the long edge |

| Backend | Task | Runtime | Cold load | 512 px p50 / p95 | 1024 px p50 / p95 | 2048 px p50 / p95 | Throughput | Peak RSS |
|---|---|---|---|---|---|---|---|---|
| rf-detr-n | detect | ort, CPU | 0.33 s | 52.1 ms / 56.4 ms | 53.1 ms / 54.9 ms | 58.5 ms / 63.5 ms | 18.7 img/s | 334 MiB |
| rf-detr-n-q4f16 | detect | ort, CPU | 0.16 s | 131.9 ms / 133.1 ms | 136.8 ms / 150.3 ms | 141.8 ms / 153.0 ms | 7.1 img/s | 402 MiB |
| efficientsam-ti | segment | ort, CPU | 0.17 s | 604.0 ms / 798.6 ms | 672.1 ms / 726.9 ms | 625.1 ms / 814.1 ms | 1.5 img/s | 1488 MiB |
| sam2.1-hiera-tiny-q4 | segment | ort, CPU | 0.29 s | 1.01 s / 1.16 s | 966.0 ms / 1.09 s | 1.11 s / 1.30 s | 1.0 img/s | 2571 MiB |
| siglip2-base-patch16-256-quantized | embed | ort, CPU | 1.31 s | 124.0 ms / 271.8 ms | 180.6 ms / 217.1 ms | 136.0 ms / 192.5 ms | 5.4 img/s | 1501 MiB |
| pp-ocrv6-tiny | read | oar-ocr on ort, CPU | 0.04 s | 80.4 ms / 131.4 ms | 98.0 ms / 120.7 ms | 112.6 ms / 177.5 ms | 9.8 img/s | 1596 MiB |
| pp-ocrv6-small | read | oar-ocr on ort, CPU | 0.15 s | 374.2 ms / 461.2 ms | 442.2 ms / 519.2 ms | 470.9 ms / 731.9 ms | 2.2 img/s | 1690 MiB |
| paddleocr-vl-1.6-metal | read | oar-ocr-vl on Candle, Metal | 21.46 s | 8.39 s / 10.83 s | 8.22 s / 8.81 s | 10.17 s / 10.56 s | 0.12 img/s | 2293 MiB |
| paddleocr-vl-1.6-cpu | read | oar-ocr-vl on Candle, CPU | 4.74 s | — | 25.53 s / 25.53 s | — | 0.04 img/s | 3491 MiB |

| Backend | Mode | Eval set | Cases | Scores | Cost / 1k images |
|---|---|---|---|---|---|
| rf-detr-n | local | generated-detect | 1 | count_accuracy 1.000, detection_ap 1.000, detection_map 1.000, detection_precision 1.000, detection_recall 1.000, region_iou 0.992 | $0.00 |
| rf-detr-n-q4f16 | local | generated-detect | 1 | count_accuracy 1.000, detection_ap 1.000, detection_map 0.850, detection_precision 1.000, detection_recall 1.000, region_iou 0.890 | $0.00 |
| efficientsam-ti | local | shapes-segment | 1 | box_hit_rate 0.667, count_accuracy 1.000, mask_iou 0.983, region_iou 0.980 | $0.00 |
| sam2.1-hiera-tiny-q4 | local | shapes-segment | 1 | box_hit_rate 1.000, count_accuracy 1.000, mask_iou 0.988, region_iou 0.992 | $0.00 |
| siglip2-base-patch16-256-quantized | local | captioned-fixtures | 4 | retrieval_image_to_text_top1 0.750, retrieval_text_to_image_top1 1.000 | $0.00 |
| pp-ocrv6-tiny | local | documents-read | 2 | field_exact_match 1.000, field_normalized_match 1.000, ocr_char_accuracy 1.000, ocr_word_accuracy 1.000 | $0.00 |
| pp-ocrv6-small | local | documents-read | 2 | field_exact_match 1.000, field_normalized_match 1.000, ocr_char_accuracy 1.000, ocr_word_accuracy 1.000 | $0.00 |
| claude-sonnet-5-5 | recorded | shapes-locate | 1 | box_hit_rate 0.600, detection_recall 1.000, region_iou 0.918 | $4.67 |
| claude-sonnet-5-5 | recorded | s5-grounding | 1 | box_hit_rate 1.000, detection_recall 1.000, grounding_hit_rate 1.000, region_iou 0.989 | $16.06 |
| claude-haiku-5-5 | recorded | shapes-locate | 1 | box_hit_rate 0.800, detection_recall 1.000, region_iou 0.934 | $0.10 |
| claude-haiku-5-5 | recorded | s5-grounding | 1 | box_hit_rate 1.000, detection_recall 1.000, grounding_hit_rate 1.000, region_iou 0.990 | $0.20 |
| nemotron-super-vl | recorded | shapes-locate | 1 | box_hit_rate 0.000, detection_recall 1.000, region_iou 0.799 | unpriced |
| nemotron-super-vl | recorded | s5-grounding | 1 | box_hit_rate 0.000, detection_recall 1.000, grounding_hit_rate 1.000, region_iou 0.824 | unpriced |
| gemma-4 | recorded | shapes-locate | 1 | box_hit_rate 0.000, detection_recall 1.000, region_iou 0.874 | unpriced |
| gemma-4 | recorded | s5-grounding | 1 | box_hit_rate 0.000, detection_recall 0.917, grounding_hit_rate 0.917, region_iou 0.713 | unpriced |
| muse-spark-1-3 | recorded | shapes-locate | 1 | box_hit_rate 0.200, detection_recall 1.000, region_iou 0.808 | $5.56 |
| muse-spark-1-3 | recorded | s5-grounding | 1 | box_hit_rate 0.583, detection_recall 1.000, grounding_hit_rate 1.000, region_iou 0.896 | $10.53 |
| sam-3.1 | recorded | shapes-concepts | 3 | box_hit_rate 1.000, detection_recall 1.000, region_iou 0.987 | $2.50 |
| claude-sonnet-5-5 | live (one live call, 2.76 s wall; load average 14.82 13.08 15.57 at its start) | shapes-locate | 1 | box_hit_rate 0.600, detection_recall 1.000, region_iou 0.927 | $4.67 |
| nemotron-super-vl | live (one live call, 3.07 s wall; load average 14.82 13.08 15.57 at its start) | shapes-locate | 1 | box_hit_rate 0.000, detection_recall 0.800, region_iou 0.755 | unpriced |
| sam-3.1 | live (one live call, 4.84 s wall; load average 14.82 13.08 15.57 at its start) | shapes-concepts | 1 | box_hit_rate 1.000, detection_recall 1.000, region_iou 0.990 | $2.50 |
| paddleocr-vl-1.6-metal | local (load average 19.00 16.20 13.85 at its start; 2 timed runs per size) | documents-read | 2 | field_exact_match 1.000, field_normalized_match 1.000, ocr_char_accuracy 0.910, ocr_word_accuracy 0.876 | $0.00 |
| paddleocr-vl-1.6-cpu | local (load average 1.83 9.49 12.21 at its start; 1 timed run per size) | documents-read | 2 | field_exact_match 1.000, field_normalized_match 1.000, ocr_char_accuracy 0.910, ocr_word_accuracy 0.876 | $0.00 |

| Backend | Model | Revision | Licence |
|---|---|---|---|
| rf-detr-n | `rf-detr-n` | `eae21cee0687` | Apache-2.0 |
| rf-detr-n-q4f16 | `rf-detr-n-q4f16` | `eae21cee0687` | Apache-2.0 |
| efficientsam-ti | `efficientsam-ti` | `1cf49585c395` | Apache-2.0 |
| sam2.1-hiera-tiny-q4 | `sam2.1-hiera-tiny-q4` | `814a066640de` | Apache-2.0 |
| siglip2-base-patch16-256-quantized | `siglip2-base-patch16-256-quantized` | `d1114256522a` | Apache-2.0 |
| pp-ocrv6-tiny | `pp-ocrv6-tiny-det` | `2ba1506c0380` | Apache-2.0 |
| pp-ocrv6-tiny | `pp-ocrv6-tiny-rec` | `2612ab37152a` | Apache-2.0 |
| pp-ocrv6-small | `pp-ocrv6-small-det` | `28fe5895c24f` | Apache-2.0 |
| pp-ocrv6-small | `pp-ocrv6-small-rec` | `b8f84f0b80c5` | Apache-2.0 |
| claude-sonnet-5-5 | `claude-sonnet-5-5` | — | provider terms |
| claude-haiku-5-5 | `claude-haiku-5-5` | — | provider terms |
| nemotron-super-vl | `nvidia/nemotron-3.5-super-vl-120b-a12b` | — | provider terms |
| gemma-4 | `google/gemma-4-31b-it` | — | provider terms |
| muse-spark-1-3 | `muse-spark-1.3` | — | provider terms |
| sam-3.1 | `sam-3.1` | — | provider terms |
| paddleocr-vl-1.6-metal | `paddleocr-vl-1.6` | `132c45f52c0a` | Apache-2.0 |
| paddleocr-vl-1.6-metal | `pp-doclayoutv3` | `e4f489dc5365` | Apache-2.0 |
| paddleocr-vl-1.6-cpu | `paddleocr-vl-1.6` | `132c45f52c0a` | Apache-2.0 |
| paddleocr-vl-1.6-cpu | `pp-doclayoutv3` | `e4f489dc5365` | Apache-2.0 |

## Reading the Results

| Backend | What the run shows |
|---------|--------------------|
| RF-DETR Nano | About 52–59 ms per image at every input size on the M4's CPU: the runtime resizes each image to the model's 384×384 input first. Both drawn objects found, region IoU 0.992 |
| RF-DETR Nano q4f16 | A 19 MB export against 108 MB, and on the CPU about 2.5 times slower than fp32 (132–142 ms); it finds both objects, with looser boxes (region IoU 0.890, COCO mAP 0.85 against 1.00) |
| EfficientSAM-Ti, SAM 2.1 Tiny q4 | Each image costs an encoder pass, about 0.6 s and 1.0 s, nearly flat across input sizes. Both segment the drawn shapes at mask IoU 0.98 or better; SAM 2.1's boxes are within 2 px for all three shapes, EfficientSAM's for two |
| SigLIP 2 base | 124–181 ms per image; every caption ranks its own fixture first, and three of four fixtures rank their own caption first |
| PP-OCRv6 tiny, small | Tiny reads a page in 80–113 ms, small in 374–471 ms; both read the report page and the invoice exactly and extract all seven invoice fields |
| PaddleOCR-VL-1.6 | On Metal 8.2–10.2 s per page after a 21 s load; on the CPU 25.5 s per page at 1024 px. Every word it reads matches; its character accuracy of 0.91 counts the chart's tick and axis labels, which it leaves to chart recognition, and its layout order, which puts the invoice total after the footer. All seven invoice fields extracted |
| Claude Sonnet 5.5, Haiku 5.5 | Every S5 click lands inside its target, at region IoU 0.99; on the drawn shapes, three (Sonnet) and four (Haiku) of five boxes are within 2 px |
| Nemotron Super VL, Gemma 4 | No box lands within 2 px of the drawn edges (region IoU 0.71–0.87); Nemotron places every S5 click inside its target, Gemma 4 all but one. NVIDIA publishes no price for its hosted endpoints, so the rows say unpriced, not free |
| Muse Spark 1.3 | Every S5 click inside its target; boxes looser than Claude's (region IoU 0.81–0.90) |
| `sam-3.1` | Each named shape at region IoU 0.987, $2.50 per 1,000 images as the catalog prices it; one live call took 4.8 s end to end |

The cloud costs are the run's metered spend: a describer's cost follows the tokens the image
becomes, so the 3024×1964 S5 screenshot costs Claude Sonnet 5.5 $16.06 per 1,000 against
$4.67 for the 960×540 shapes image. Local backends record their units at zero cost.

The run shared the machine with other work: the local rows were measured with the
project's other builds held off, and the load average each run started under is in the
machine table or the row. Load moves the numbers: an earlier run on the same machine, started at a
load average of 48, measured RF-DETR Nano at 76 ms p50 against 52 ms here.

## Related

- [Vision Evals](evals.md)
- [Local ONNX backends](onnx.md)
- [Document readers](document-readers.md)
- [Cloud vision backends](cloud.md)
