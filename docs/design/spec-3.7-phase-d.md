# Spec 3.7 phase D — spatial perception and robotics bridges: research spike

| | |
|---|---|
| **Status** | Design note for review, 2026-11-23; research only, no product code |
| **Spec** | [`spec-3.7-vision.md`](spec-3.7-vision.md) §7.19 (spatial), §12 phase D, scenarios S20 and S21, §9 (security invariants) |
| **Grounding** | `origin/main` @ `b526a163c` for every `file:line` reference |
| **Sources** | Primary sources — model cards, licence files, repositories, papers, REPs — read on 2026-11-23 and listed under [References](#references). † marks a figure that rests on arithmetic or one secondary source; **UNVERIFIED** marks a claim no primary source settled |

Phase D starts with a research spike because depth, 3D, pose, ROS 2 and vision-language-action
(VLA) models were outside the landscape report behind spec 3.7 (§4.3). This note answers what
the spike was asked: which depth estimators and pose models phase D can ship under D-7, what
the spatial contracts look like so they normalise as S5 does, which ROS 2 client a Rust crate
can build on without a ROS installation in CI, what a "VLA bridge" should mean, and how S21's
motion request stays on the governed path. Section 9 lists the decisions the owner must make.

## 1. Recommendations

| # | Recommendation | Section |
|---|---|---|
| D1 | The fiducial's own distance comes from geometry, not from a depth model: corner detection plus perspective-n-point (PnP) with calibrated intrinsics meets S20's 5% at 3 m with a marker of 100 mm or more. A depth model is tested on held-out targets, never on the fiducial that scaled it | [§3.4](#34-meeting-s20) |
| D2 | Local depth backends, all permissive: **MoGe-2 ViT-S** (MIT code and weights; metric point map; estimates or takes the field of view) as the candidate default, **Depth Anything V2 Small** (Apache-2.0; relative) as the second backend, **DA3METRIC-LARGE** (Apache-2.0; metric given the focal length) as an opt-in large model. The default is chosen by the benchmark (D-13), not by this note | [§3.5](#35-recommendation) |
| D3 | Excluded from the built-in manifest: Depth Pro and Depth Pro 2 (Apple licences, research-only weights on Hugging Face), UniDepth (CC BY-NC 4.0), Depth Anything V2 Base/Large/Giant and DA3 Large/Giant/Nested (CC BY-NC 4.0), DepthCrafter (research only), DepthART (non-commercial file and teacher). Metric3D v2 (weights licence unstated) and Marigold (RAIL++-M) only through a user's own manifest | [§3.1](#31-candidates) |
| D4 | One camera type, `CameraIntrinsics`, carried through the existing `TransformChain`; a `DepthEstimator` trait answering a raw raster in its own frame, normalised by the runtime into a `DepthMap` over source pixels; `Region3` in the source camera's optical frame (REP 103); pose as the existing `Keypoints` shape plus a `Pose3` for 6-DoF | [§4](#4-contracts) |
| D5 | ROS 2 through a new crate, **`adk-ros2`**, outside `adk-vision`, on a client that builds from crates.io alone, with the handful of messages it needs vendored and serialised as CDR — so the PR tier builds and tests it without ROS. Interoperability with real ROS 2 nodes runs in a nightly job inside a `ros:jazzy` container | [§5](#5-ros-2) |
| D6 | Detections leave as `vision_msgs/Detection2DArray` (and `Detection3DArray` once depth lands) whose header names the camera's optical frame and the capture time; regions only, never pixels, by default; a frame published to ROS is an egress through the privacy step | [§5.3](#53-detections-out) |
| D7 | A robot-side motion request is accepted and deferred, becomes a governed call of a new never-waivable kind, and is held with the frame, regions and projected target as evidence. Only `ToolAuthorization::Execute` constructs the `MotionGrant` the bridge needs to send anything that moves, as only the egress gate constructs a `ReleasedImage` (V-I1) | [§5.4](#54-requests-in-a-held-governed-call) |
| D8 | A "VLA bridge" proposes and never executes on its own: it turns a policy server's action chunk into evidence on a held motion call, or asks a person for a bounded task grant (duration, workspace box, speed) under which the robot's own controller runs the policy and the kill switch cancels it | [§6](#6-vla-bridges) |
| D9 | Phase D is about 11 engineer-weeks, up from 9.5: the bridge is two crates' worth of work (vendored messages and the transport), and S20 needs a recorded calibration scene | [§7](#7-plan) |

## 2. What phase D must prove

| Scenario | Pass condition (spec §5) | What it implies |
|---|---|---|
| **S20** Depth and pose | Distance to the fiducial within 5% at 1–3 m; keypoints share the region contract and normalise like S5 | Calibrated intrinsics; a 3D contract that knows the camera; a test that does not measure the depth model against the fiducial that scaled it ([§3.4](#34-meeting-s20)); keypoints through `RegionNormalizer`, which main already does for 2D ([§4.4](#44-pose-and-keypoints)) |
| **S21** Perception out, action in through the gate | Detections arrive as standard messages with source-frame metadata; the motion request reaches the agent as a governed tool call and is held; nothing moves without the governed path | A ROS 2 transport CI can build; message mapping with `frame_id` and stamps; a held call whose approval binds to the exact motion; a type that makes "move without `Execute`" impossible to write ([§5.4](#54-requests-in-a-held-governed-call)) |

Spec 3.7 non-goal N8 stands: phase D gives no real-time control loop and no safety guarantee
for a robot. The robot's own controllers, safety-rated stops and the standards that govern
them — ISO 10218-1 and -2, revised in 2025 with the collaborative requirements of ISO/TS
15066 folded in ([ISO](https://www.iso.org/standard/73934.html); [The Robot Report](https://www.therobotreport.com/iso-10218-industrial-robot-safety-standard-receives-major-overhaul/)†)
— stay authoritative, as `computer-use-mcp` stays authoritative for desktop policy
([`adk-computer-use/src/lib.rs:13`](../../adk-computer-use/src/lib.rs)). What phase D
guarantees is narrower and testable: nothing the agent or its bridge does sends a motion
without a decision on the governed path.

## 3. Depth

### 3.1 Candidates

Licences were read from each model card, repository licence file and README. Where they
disagree the conservative reading applies and the row says so. "Class" is the
`LicenceClass` of spec §7.4.

| Model | Params | Code | Weights | Class | Output | Intrinsics | ONNX export |
|---|---|---|---|---|---|---|---|
| **Depth Anything V2 Small** | 24.8M | Apache-2.0 | Apache-2.0 | Permissive | Relative inverse depth (affine-invariant) | Not used | `onnx-community/depth-anything-v2-small`: fp32 99.1 MB, int8 27.3 MB, q4f16 19.1 MB (Apache-2.0) |
| Depth Anything V2 Metric Small (Hypersim, VKITTI2) | 24.8M | Apache-2.0 | Apache-2.0 on the `.pth` repos; the `-hf` indoor repos carry no tag | Permissive (Small only) | Metric, to 20 m indoor or 80 m outdoor | Not used | `fabio-sim/Depth-Anything-ONNX` v2.0.0: indoor and outdoor, 99 MB each |
| Depth Anything V2 Base, Large, Giant | 97.5M, 335.3M, — | Apache-2.0 | "CC-BY-NC-4.0" (README) | NonCommercial | Relative | — | — |
| DA3-SMALL, DA3-BASE | 80M, 120M | Apache-2.0 | Apache-2.0 | Permissive | Relative depth with rays; camera pose across views | Predicted; optional input | `onnx-community/depth-anything-v3-small`, 104.7 MB; source checkpoint **UNVERIFIED** |
| **DA3METRIC-LARGE** | 350M | Apache-2.0 | Apache-2.0 | Permissive | Metric: `metric_depth = focal * net_output / 300` | Focal length required | `Heliosoph/da3metric-large-onnx`: fp16 669 MB, fixed 504×504 input |
| DA3MONO-LARGE | 350M | Apache-2.0 | Apache-2.0 | Permissive | Relative depth | Not used | — |
| DA3-LARGE(-1.1), -GIANT, -NESTED | 0.35–1.40B | Apache-2.0 | CC BY-NC 4.0 (LARGE-1.1's card says Apache 2.0; the repository's table says CC BY-NC 4.0) | NonCommercial | — | — | — |
| **MoGe-2** ViT-S, -B, -L | 35M, 104M, 326M | MIT (DINOv2 parts Apache-2.0) | MIT | Permissive | Metric point map, depth, normals | Estimates the field of view; takes it when known | Official `Ruicheng/moge-2-vits-normal-onnx`, 140.9 MB, raw forward pass only (no licence tag on the ONNX repository; the weights repository is MIT) |
| MoGe-3 | 370M, 1.25B | MIT | MIT | Permissive | Metric | As MoGe-2 | None; "macOS is not supported" (needs Triton) |
| ZoeDepth | ~345M (1.38 GB) | MIT | MIT | Permissive | Metric, trained on NYU and KITTI | Not used | — |
| Video Depth Anything Small, Metric Small | 28.4M | Apache-2.0 | Apache-2.0 (Base and Large CC BY-NC 4.0) | Permissive (Small) | Relative or metric, temporally consistent | Not used | — |
| Prompt Depth Anything | 25.1M–340M | Apache-2.0 | Apache-2.0 | Permissive | Metric, given a low-resolution LiDAR depth prompt | — | — |
| Metric3D v2 S, L, g | 412M (L) | BSD-2-Clause, "for further commercial inquiries, please contact…" | Not stated | Unknown: a user's manifest only | Metric through a canonical camera | Required (`cam_in`) | `onnx-community/metric3d-vit-small`, 150.7 MB (converter's tag CC0-1.0) |
| UniDepth v1, v2 | — | CC BY-NC 4.0 | No tag | NonCommercial | Metric, predicts intrinsics | Optional | Official export script |
| Depth Pro | 504M | Apple sample-code licence (no patent grant) | Same licence (CDN); "apple-amlr", research only (Hugging Face) | Not permissive | Metric, estimates focal length | Not needed | `onnx-community/DepthPro-ONNX`, fp16 1.9 GB |
| Depth Pro 2 (2026-11-19) | 24M–872M | Apple sample-code licence | "for research use only" | NonCommercial | Metric | — | Core ML packages |
| Marigold | UNet 3.46 GB | Apache-2.0 | "RAIL++-M" (README; one card says apache-2.0) | UseRestricted at best | Affine-invariant | — | — |
| DepthCrafter | — | — | "academic, research and education purposes" | NonCommercial | Relative video depth | — | — |
| DepthART | — | CC BY 4.0, one file CC BY-NC 4.0 | Distilled from Depth Anything V2 Large (CC BY-NC 4.0); the Hugging Face tag says apache-2.0 | Excluded until clarified | Metric | Required | 48.9 MB |

### 3.2 Accuracy: metric against relative at 1–3 m

δ1 counts pixels within 25% of the truth, so it says nothing about a 5% requirement. The
figures that do:

| Model | NYUv2 AbsRel, zero-shot | Middlebury (1.3–7.6 m) AbsRel, raw | Middlebury, scale from the truth | Source |
|---|---|---|---|---|
| Depth Anything V2 Small, relative | 0.053 (scale and shift fitted per image) | — | — | [DAV2 paper](https://arxiv.org/abs/2406.09414), Table 2 |
| Depth Anything V2 Metric-Hypersim Small | — | 29.9% (predicts too close) | 8.9% | [Orin benchmark](https://arxiv.org/abs/2610.27199), Table 3 |
| DA3METRIC-LARGE | 0.070 | 12.3% with the true focal length | — | [DA3 paper](https://arxiv.org/abs/2511.10647), Table 11; Orin benchmark |
| MoGe-2 ViT-L | 7.33% without intrinsics, 6.46% with | 19–33% | 3.8% | [MoGe-2 paper](https://arxiv.org/abs/2507.02546) B.5; Orin benchmark |
| Metric3D v2 L | 0.067 | 57–60% | 4.0–6.5% | DA3 Table 11; Orin benchmark |
| Depth Pro | 0.093 | 12.0% (true focal), 19.0% (its own) | — | DA3 Table 11; Orin benchmark |
| UniDepthV2 | 0.064 | — | — | DA3 Table 11 |

The October 2026 Jetson AGX Orin benchmark states the conclusion: the best raw results reach
about 12% AbsRel, and "only 29% and 31% of their pixels are within 5% of the true depth"; with
the scale taken from another source, "such as a known object size", MoGe-2-L gives the best
shapes at 3.8%, and "even the best of these keeps 27% of its pixels more than 5% from the true
depth" ([arXiv 2610.27199](https://arxiv.org/abs/2610.27199)). No permissive model measures
per-pixel distance to 5% zero-shot. Per-region statistics — the median over a mask — are
steadier than per-pixel error, but no source measured them; the phase D benchmark does.

Aligning relative depth to metric scale is standard practice: MiDaS aligns "in scale and shift
for each image… in inverse-depth space based on the least-squares criterion"
([arXiv 1907.01341](https://arxiv.org/abs/1907.01341)); VI-Depth does the same against sparse
metric points, from about 150 of them ([arXiv 2303.12134](https://arxiv.org/abs/2303.12134));
DA3 aligns to sparse depth "via a robust ransac scale–shift procedure".

### 3.3 Latency expectations

| Model | Figure | Hardware | Source |
|---|---|---|---|
| Depth Anything V2 Small, Core ML F16, 518×396 | 32.8 ms; 24.58 ms | M1 Max; M3 Max (Neural Engine) | [`apple/coreml-depth-anything-v2-small`](https://huggingface.co/apple/coreml-depth-anything-v2-small) |
| Depth Anything V2 S, B, L, ONNX Runtime CUDA, 518² | 13.3, 29.3, 83.2 ms | RTX 4080 | [`fabio-sim/Depth-Anything-ONNX`](https://github.com/fabio-sim/Depth-Anything-ONNX) |
| Depth Anything V2 Small, int8 | p50 728 ms | Phone CPU (ExecuTorch, XNNPACK) | Arm model page† |
| DA3METRIC-LARGE | 33 ms | Jetson AGX Orin, TensorRT FP16 | Orin benchmark |
| MoGe-2 ViT-L | 29 ms FP16, 82 ms FP32 | A100 | MoGe-2 paper |
| Depth Pro | 0.3 s for 2.25 MP | V100 | [Depth Pro paper](https://arxiv.org/abs/2410.02073) |

No source gives a desktop CPU time through ONNX Runtime, or a time through `ort`'s Core ML
execution provider, for any candidate. Arithmetic on the ViT-S encoder at 518² (1,369 tokens)
puts it near 100 GFLOPs per frame, so a laptop CPU runs a ViT-S model at a few frames per
second and a ViT-L model at about a second per frame or slower†. Depth therefore runs on
demand — on a watch event, a measurement or a motion request — and never in the Inspector's
5 fps per-frame path. The benchmark harness (spec §7.17) records the real figures before any
default is set.

### 3.4 Meeting S20

**The fiducial's distance needs no depth model.** A square marker of side `S` at range `Z`
spans `s = f·S/Z` pixels for a focal length `f`. With its four corners located, PnP gives the
marker's full pose; its translation is in the marker's units
([OpenCV ArUco tutorial](https://github.com/opencv/opencv/blob/4.x/doc/tutorials/objdetect/aruco_detection/aruco_detection.markdown)).
The range error is `√((σs/s)² + (σf/f)² + (σS/S)²)`. For a 1920×1080 camera with `f ≈ 1400 px`,
a focal length calibrated to 0.5% and a print tolerance of 0.5 mm†:

| Marker side | 1 m | 2 m | 3 m (corners ±0.3 px) | 3 m (corners ±1 px) |
|---|---|---|---|---|
| 50 mm | 1.3% | 1.6% | 2.1% | **6.2%** |
| 100 mm | 0.8% | 0.9% | 1.2% | 3.1% |
| 150 mm | 0.6% | 0.7% | 0.9% | 2.1% |

A 100 mm marker or larger meets 5% across 1–3 m with margin. Two conditions carry the result:
the intrinsics must be calibrated (a focal length a model estimates is not enough: Depth Pro's
Middlebury error rises from 12.0% to 19.0% on its own estimate), and the marker must be
detected to sub-pixel corners, which AprilTag and ArUco detectors do.

**Testing a depth model at the fiducial that scaled it is circular.** If the fiducial sets the
scale, the depth model's answer at the fiducial is correct by construction. The honest test
scales on one target and measures another.

**A single fiducial cannot scale a relative disparity model.** Depth Anything V2's relative
output is affine-invariant inverse depth: metric inverse depth is `a·output + b`, two unknowns.
Every corner of one planar marker lies at about one depth, so the fit cannot separate `a` from
`b`. Three ways out:

1. **A metric or shift-free model with a scale-only fit.** MoGe-2 and DA3METRIC-LARGE answer in
   metres (up to a scale error), so one anchor fixes the scale. This is the Orin benchmark's
   "known object size" route, at 3.8–4.0% AbsRel on Middlebury.
2. **Anchors at two or more depths.** Two markers at different ranges, or several.
3. **A plane as the anchor.** A marker lying on the conveyor or floor gives the plane's pose,
   so every pixel of the plane's mask is an anchor at a different depth. This needs the plane's
   mask (a fixed zone or a segmentation) and is the natural fit for the Inspector.

**Recommended S20 pass conditions**, replacing the single line in spec §5 (owner decision O1):

| # | Condition | Gated |
|---|---|---|
| a | The range to the fiducial by PnP is within 5% of the tape-measured range at 1.0, 1.5, 2.0, 2.5 and 3.0 m, with a 150 mm marker and calibrated intrinsics | Yes |
| b | Scaled on marker A, each depth backend reports the median range over held-out marker B, at another depth, within 5% | Yes for the anchored route; the threshold is confirmed by the first benchmark run (D-13) |
| c | Each metric backend's zero-shot range to marker B is reported with its error | No — reported |
| d | RF-DETR keypoints and the depth map normalise like S5: every keypoint lands within 2 px of ground truth in source pixels through a random transform chain, and depth sampled at a source pixel equals depth sampled at its image through the chain | Yes |
| e | Lengths measured out of the fiducial's plane (a part raised on a block) are within 5% once depth is used, where today's planar calibration reports them wrongly | Yes |

### 3.5 Recommendation

- **Built-in manifest:** MoGe-2 ViT-S (MIT) as the candidate default, Depth Anything V2 Small
  (Apache-2.0) as the second backend, DA3METRIC-LARGE (Apache-2.0) as an opt-in large metric
  model. MoGe-2's ONNX repository has no licence tag although its weights repository is MIT,
  so phase D either records the export's provenance against the MIT weights or exports from
  them itself, under spec Q6's redistribution review.
- **Not built-in:** the excluded rows of §3.1. A user may add Metric3D v2 through their own
  manifest; its weights licence is unstated and its README asks commercial users to make
  contact.
- **Fiducials:** AprilTag (BSD-2-Clause) or ArUco, through a pure-Rust detector where one is
  mature, otherwise a port; PnP in Rust ([§4.5](#45-fiducial-pose)).
- **Depth sensors:** a robot with an RGB-D camera already has metric depth. A
  `DepthEstimator` over a ROS 2 depth image topic (REP 118: 32-bit float metres, or 16-bit
  millimetres with 0 as invalid) is the third backend, and the one S21's robot would use.
