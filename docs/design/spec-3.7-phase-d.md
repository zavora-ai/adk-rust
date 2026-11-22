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
| D9 | Phase D is about 12 engineer-weeks after this spike, against the spec's 8: the camera model and depth alignment were not in the spec's estimate, and a ROS-free CI needs vendored messages with tested type hashes | [§7](#7-plan) |

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
| ZoeDepth | — (1.38 GB checkpoint) | MIT | MIT | Permissive | Metric, trained on NYU and KITTI | Not used | — |
| Video Depth Anything Small, Metric Small | 28.4M | Apache-2.0 | Apache-2.0 (Base and Large CC BY-NC 4.0) | Permissive (Small) | Relative or metric, temporally consistent | Not used | — |
| Prompt Depth Anything | 25.1M–340M | Apache-2.0 | Apache-2.0 | Permissive | Metric, given a low-resolution LiDAR depth prompt | — | — |
| Metric3D v2 S, L, g | 412M (L) | BSD-2-Clause, "for further commercial inquiries, please contact…" | Not stated | Unknown: a user's manifest only | Metric through a canonical camera | Required (`cam_in`) | `onnx-community/metric3d-vit-small`, 150.7 MB (converter's tag CC0-1.0) |
| UniDepth v1, v2 | — | CC BY-NC 4.0 | No tag | NonCommercial | Metric, predicts intrinsics | Optional | Official export script |
| Depth Pro | 504M | Apple sample-code licence (no patent grant) | Same licence (CDN); "apple-amlr", research only (Hugging Face) | Not permissive | Metric, estimates focal length | Not needed | `onnx-community/DepthPro-ONNX`, fp16 1.9 GB |
| Depth Pro 2 (2026-11-19) | 24M–872M | Apple sample-code licence | "for research use only" | NonCommercial | Metric | — | Core ML packages |
| Marigold | UNet 3.46 GB | Apache-2.0 | "RAIL++-M" (README; one card says apache-2.0) | UseRestricted at best | Affine-invariant | — | — |
| DepthCrafter | — | — | "academic, research and education purposes" | NonCommercial | Relative video depth | — | — |
| DepthART | — | CC BY 4.0, one file CC BY-NC 4.0 | Trained on supervision Depth Anything V2 Large generated (CC BY-NC 4.0); the Hugging Face tag says apache-2.0 | Excluded until clarified | Metric | Required | 48.9 MB |

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
a focal length calibrated to 0.5%, a print tolerance of 0.5 mm and corners located to ±0.3 px†:

| Marker side | 1 m | 2 m | 3 m | 3 m, corners only to ±1 px |
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
| e | Lengths measured out of the fiducial's plane (a part raised on a block) are within 5% once depth is used, where today's planar calibration reports them wrongly | Yes; the threshold confirmed with (b)'s |

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

## 4. Contracts

The types below show shape and responsibility, as spec §7 does; names may change in review and
none of them compiles today. Each follows the rule main already enforces in 2D: a backend
answers in its own frame, and the runtime normalises the answer into source pixels — and, for
3D, into the source camera's optical frame — with the chain that produced it recorded.

### 4.1 The camera through the transform chain

Main has no camera type. `Calibration` is a planar scale in pixels per millimetre, valid only
in the fiducial's plane ([`measure/mod.rs:159`](../../adk-vision/src/measure/mod.rs),
`MEASUREMENT_VALIDITY`). Every 3D quantity needs the camera that took the source image:

```rust
use serde::{Deserialize, Serialize};

/// The pinhole camera that took a source image, in that image's pixels.
///
/// Coordinates follow adk-vision's continuous convention: the top-left corner of the
/// top-left pixel is (0, 0), so pixel (i, j) has its centre at (i + 0.5, j + 0.5).
/// OpenCV and ROS `CameraInfo` put pixel centres at integers; `from_camera_info` adds
/// 0.5 to cx and cy, and `to_camera_info` subtracts it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraIntrinsics {
    pub fx: f64,
    pub fy: f64,
    pub cx: f64,
    pub cy: f64,
    /// The image size the intrinsics hold for.
    pub width: u32,
    pub height: u32,
    /// Images are undistorted before any 3D lift.
    pub distortion: Distortion,
    /// Where the numbers came from, which sets their uncertainty.
    pub source: IntrinsicsSource,
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "model")]
pub enum Distortion {
    None,
    /// ROS `plumb_bob`: k1, k2, t1, t2, k3.
    PlumbBob { d: [f64; 5] },
    /// ROS `equidistant` (fisheye): k1..k4.
    Equidistant { d: [f64; 4] },
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum IntrinsicsSource {
    /// A calibration run, with its reprojection error and the images it used.
    Calibrated { rms_px: f64, images: Vec<Digest> },
    /// A ROS `sensor_msgs/CameraInfo` from the named optical frame.
    CameraInfo { frame_id: String },
    /// A focal length a model estimated (MoGe-2, DA3, Depth Pro); not enough for S20.
    Estimated { backend: BackendId },
}
```

- **Through the chain.** `TransformChain` gains `intrinsics_to_target`: a crop subtracts its
  offset from (cx, cy); a resize scales (fx, cx) and (fy, cy) by its two factors; a pad adds its
  left and top; a quarter turn swaps the axes and rotates the optical frame about z. Every step
  of [`transform.rs:166`](../../adk-vision/src/geometry/transform.rs) is affine in pixel
  coordinates, so the mapping is exact.
- **Focal-dependent backends.** A backend that needs a focal length — DA3METRIC-LARGE's
  `focal * net_output / 300`, Metric3D's canonical camera — receives the intrinsics of the image
  it is handed, never the source's. A half-size image with the source's focal length scales
  every depth by the same factor of two. The DA3 metric export on Hugging Face squashes every
  image to 504×504, so fx and fy change by different factors there.
- **Where 3D lives.** Every 3D result is in the *source* camera's optical frame, REP 103's
  "z forward, x right, y down" ([REP 103](https://reps.openrobotics.org/rep-0103/)). A rotated
  sent image never leaks its rotated frame into a stored result.

### 4.2 `DepthEstimator`

```rust
use async_trait::async_trait;

#[async_trait]
pub trait DepthEstimator<I: VisionInput = SentImage>: Send + Sync {
    fn descriptor(&self) -> &BackendDescriptor;

    /// A depth raster in the descriptor's frame, as the model produced it.
    async fn estimate(&self, cx: &VisionCx, image: &I, request: &DepthRequest)
        -> Result<RawDepth>;
}

pub struct DepthRequest {
    /// The intrinsics of the image the backend is handed, when known.
    pub intrinsics: Option<CameraIntrinsics>,
}

pub struct RawDepth {
    pub width: u32,
    pub height: u32,
    /// Row-major; NaN where the model gives no value.
    pub values: Vec<f32>,
    pub kind: DepthKind,
    /// A focal length the model estimated, in the handed image's pixels.
    pub focal_px: Option<f64>,
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DepthKind {
    /// Metres along the optical axis (REP 118), possibly off by an unknown scale.
    Metric,
    /// Depth correct up to one global scale.
    UpToScale,
    /// Inverse depth up to scale and shift: Depth Anything V2's relative output.
    AffineInverse,
}
```

The runtime turns a `RawDepth` into a `DepthMap`, in four steps that mirror
`RegionNormalizer` ([`normalize.rs:38`](../../adk-vision/src/geometry/normalize.rs)):

1. **Undo the backend's frame.** Remove letterbox padding and rotation, so the raster covers the
   source image (or the crop that was sent) through crop and resize steps only. Nearest-valid
   resampling never interpolates across a NaN.
2. **Align.** Fit the raw values to anchors when the kind needs it: a scale for `Metric` and
   `UpToScale`; scale and shift in inverse-depth space by least squares for `AffineInverse`,
   which is refused with fewer than two anchor depths ([§3.4](#34-meeting-s20)). Anchors come
   from fiducial poses, a known plane, a depth sensor or sparse points, each with its
   uncertainty.
3. **Store.** The raster is saved as an artifact by digest: a 16-bit PNG in millimetres with 0
   as invalid, REP 118's raw representation, which the `image` crate writes losslessly; a
   65.535 m ceiling is ample for 1–3 m work.
4. **Answer per region.** `RegionDepth` reports the median depth and range over a region's
   pixels, the valid fraction and an uncertainty.

```rust
/// A depth map over a source image: an artifact, the chain from the source image to its
/// raster, and how its scale was fixed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DepthMap {
    pub image: Digest,
    pub raster: ImageRef,
    pub chain: TransformChain,
    pub intrinsics: Option<CameraIntrinsics>,
    pub scale: DepthScale,
    pub origin: RegionOrigin,
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum DepthScale {
    /// The backend's own metric scale, uncertainty from its recorded benchmark error.
    Model { abs_rel: f64 },
    /// Fitted to anchors: the fit's residual and the anchors' own uncertainty.
    Anchored { anchors: Vec<DepthAnchor>, residual_rel: f64 },
}

/// Something of known depth in the image, with its uncertainty.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum DepthAnchor {
    /// A fiducial's pose by PnP: its corners, at their depths.
    Fiducial { region: usize, pose: Pose3, uncertainty_m: f64 },
    /// A plane through a fiducial (the belt, the floor): every pixel of its mask.
    Plane { mask: usize, normal: [f64; 3], offset_m: f64, uncertainty_m: f64 },
    /// A registered depth sensor's raster.
    Sensor { raster: Digest, uncertainty_rel: f64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionDepth {
    /// Index of the region in its `RegionSet`.
    pub region: usize,
    /// Median depth along the optical axis, metres.
    pub depth_m: f64,
    /// Median Euclidean range from the optical centre, metres: what "distance" means in S20.
    pub range_m: f64,
    /// One standard uncertainty of `range_m`.
    pub uncertainty_m: f64,
    /// Share of the region's pixels with a valid depth.
    pub valid_fraction: f32,
}
```

`vision_measure` gains depth: a mask measured with a `DepthMap` and intrinsics is lifted to 3D
and measured there, so a part raised off the belt no longer measures wrongly; without depth it
keeps today's planar answer and `MEASUREMENT_VALIDITY`. `VisionTask` gains `Depth` and `Pose`
(it is `#[non_exhaustive]`, [`manifest/mod.rs:30`](../../adk-vision/src/manifest/mod.rs)).

### 4.3 `Region3` and `Pose3`

```rust
/// Something located in 3D, in the source camera's optical frame (REP 103: x right,
/// y down, z forward; metres).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Region3 {
    /// The 2D region it was lifted from, in source pixels.
    pub region: Region,
    /// The camera's optical frame, for example `wrist_camera_optical_frame`.
    pub frame_id: String,
    pub intrinsics: CameraIntrinsics,
    pub shape: Shape3,
    /// The depth map it was lifted with, or `None` for a fiducial pose.
    pub depth: Option<Digest>,
    pub uncertainty_m: f64,
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Shape3 {
    Point { at: [f64; 3] },
    /// vision_msgs `BoundingBox3D`: centre pose and size in metres.
    Box { pose: Pose3, size: [f64; 3] },
    Keypoints { skeleton: String, points: Vec<Keypoint3> },
}

/// A position and orientation (quaternion x, y, z, w), as geometry_msgs `Pose`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pose3 {
    pub translation: [f64; 3],
    pub rotation: [f64; 4],
}

/// A keypoint in 3D: MediaPipe's world landmarks, or a 2D keypoint lifted with depth.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keypoint3 {
    pub at: [f64; 3],
    pub visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}
```

`Region3` keeps its 2D region, so the existing `RegionSet` schema, surfaces and evidence cards
draw it as they draw any region, and a `Region3Set` adds the 3D fields beside it. Frames other
than the camera's — a robot's `base_link`, `map` — are the ROS side's business through its TF
tree (REP 105); adk-vision does not hold a transform tree.

### 4.4 Pose and keypoints

Main already carries `Shape::Keypoints { skeleton, points: [Keypoint { x, y, visible, score }] }`
([`geometry/mod.rs:182-217`](../../adk-vision/src/geometry/mod.rs)), the region-set schema
publishes it, and `RegionNormalizer` maps every keypoint from any frame to source pixels
([`normalize.rs:110`](../../adk-vision/src/geometry/normalize.rs)). A keypoint detector is a
`Detector` whose raw regions carry keypoints; no new trait is needed. Four gaps:

| Gap | Today | Change |
|---|---|---|
| A keypoint off the image | `clamp` moves it to the edge and keeps `visible: true` ([`normalize.rs:203`](../../adk-vision/src/geometry/normalize.rs)) | A clamped keypoint becomes `visible: false`, so a limb outside the frame is never drawn or measured at the border |
| Skeletons are free strings | `skeleton: String` | A skeleton registry in the schema (`coco-17`, `halpe-26`, `coco-wholebody-133`, `mediapipe-33`, and user-named ones from a manifest) with point names and edges, so surfaces draw limbs without depending on adk-vision |
| A pose detection has a box and keypoints | One shape per region | `Keypoints` gains an optional `bbox`; RF-DETR Keypoint returns both (`xyxy` and `xy`) |
| No task or manifest entry | `VisionTask` has no pose | `VisionTask::Pose`; RF-DETR Keypoint in the manifest |

**Models.** Read from repositories, licence files and cards:

| Model | Size | Skeleton | Code / weights | ONNX | Verdict |
|---|---|---|---|---|---|
| **RF-DETR Keypoint (Preview)**, rfdetr 1.12.1 | 40.7M, 576×576; COCO AP 71.8 at 9.7 ms on a T4 | COCO-17; fine-tunes to any number of points | Apache-2.0: "RF-DETR Keypoint [is] released under the Apache 2.0 license" ([README](https://github.com/roboflow/rf-detr)) | `model.export(format="onnx")`; file size not published | Default, with the preview's caveat that "checkpoint weights may change"; pinned by revision |
| RTMO s/m/l (mmpose) | ONNX 36.4–163 MB | COCO-17, one stage | Apache-2.0 | OpenMMLab SDK zips | Alternate |
| RTMPose, RTMW | t–x | COCO-17, Halpe-26, 133-point whole body | Apache-2.0; training-set terms unverified | OpenMMLab SDK zips; needs a person detector | Alternate |
| ViTPose, ViTPose++ | 33.1M–125.4M | COCO-17 | Apache-2.0 | `onnx-community/vitpose-base-simple`, fp32 343.9 MB | Alternate |
| MediaPipe Pose Landmarker | 5.78–30.66 MB | 33 landmarks with world coordinates in metres | Apache-2.0 | `qualcomm/MediaPipe-Pose-Estimation`, 15.8 MB | Alternate; native 3D keypoints |
| MoveNet | — | COCO-17 | Apache-2.0 | None first-party | Not without an export |
| YOLO11/YOLO26-pose | — | COCO-17 | AGPL-3.0 | — | Excluded (N5) |
| Sapiens, Sapiens2 | 0.3B+ | 308 | CC BY-NC 4.0; Sapiens2 forbids use "for purposes of surveillance" | — | Excluded |
| FoundationPose (6-DoF objects) | — | — | NVIDIA Source Code License: "non-commercially… for research or evaluation purposes only" | — | Excluded |

**People.** Human pose processes people, so N2 and D-12 bound it: skeletons are anonymous,
carry no face or identity, share the track rules of V-I8, and the privacy step still blurs faces
before any write. The use phase D needs is a safety zone — stop or hold when a person's
keypoints enter a robot's workspace — not a description of a person (owner decision O7).

### 4.5 Fiducial pose

S20 needs a marker's corners to sub-pixel accuracy and its pose by PnP. Pure-Rust options, from
crates.io and the repositories:

| Crate | Version (date) | Licence | Does | Note |
|---|---|---|---|---|
| `kornia-apriltag` | 0.2.0 (2026-10-12) | Apache-2.0 | AprilTag 16h5, 25h9, 36h11, Circle21h7, Circle49h12 | Detection only; pose from `kornia-3d` |
| `kornia-3d` | 0.2.0 (2026-09-26) | Apache-2.0 | PnP (AP3P, EPnP, RANSAC, refinement); pinhole and fisheye cameras | — |
| `locus-core` | 0.11.0 (2026-11-09) | MIT OR Apache-2.0 | AprilTag and ArUco, ChArUco boards, 6-DoF pose (IPPE-Square + LM) | "Experimental: pre-1.0, not recommended for production yet" |
| `sqpnp` | 0.4.0 (2026-02-25) | MIT OR Apache-2.0 | SQPnP | — |
| `apriltag` / `apriltag-sys` | 0.4.0 | BSD-2-Clause | Bindings to the C reference library (BSD-2-Clause) | Links C; a fallback |
| `opencv` | 0.102.0 | MIT (links system OpenCV, Apache-2.0) | ArUco, `solvePnP`, calibration | A system dependency; not for the default build |
| `opencv-ros-camera` | 0.17.0 (2026-08-07) | MIT OR Apache-2.0 | OpenCV/ROS camera models | For `CameraInfo` round trips |

The recommendation is `kornia-apriltag` with `kornia-3d` (one maintained family, Apache-2.0,
pure Rust) behind a `fiducial` feature, with `locus-core` evaluated as the alternate in the
benchmark; `Calibration::from_fiducial_pose` then gives both the planar scale main has and the
marker's `Pose3`. `lambda-twist` (P3P) is MPL-2.0 and is not needed.

### 4.6 Normalising like S5, and conformance

S5 proves 48 regions from four frames land within 2 px in source pixels and that random chains
round-trip within 0.5 px. Phase D adds, as property tests (`proptest`, 100+ cases) and
conformance checks:

| Check | Statement |
|---|---|
| Intrinsics through the chain | For random chains and random points in front of the camera, projecting with the transformed intrinsics equals transforming the source projection, within 1e-6 px; lifting back returns the point |
| CameraInfo round trip | `from_camera_info` then `to_camera_info` returns K, D and P exactly; cx and cy move by 0.5 and back |
| Depth through the frame | Depth sampled at a source pixel equals depth sampled at its image in the raw raster through the chain, for letterboxed, stretched and rotated inputs |
| Alignment | On a synthetic scene of known depth, the scale-only fit recovers a known scale exactly; the scale-and-shift fit recovers both from two depths and refuses one |
| Keypoints | Visible keypoints in any frame land within 2 px of ground truth; a keypoint mapped outside the image comes back invisible |
| Region3 | A `Region3`'s 2D region equals the projection of its 3D shape within 0.5 px |
| `depth_conformance_tests!` | Units and zero cost recorded; no egress from local backends; cancellation within one model call; oversized input refused with the limit; `DepthKind` honest (an `AffineInverse` backend never reports `Metric`) |

## 5. ROS 2

### 5.1 Client options

The constraint that decides: CI runs on plain Ubuntu, macOS and Windows runners with no ROS
installation (`AGENTS.md`, "CI cost tiers"). Versions and dates are from crates.io; licences
from each repository.

| Option | Latest | Licence | Builds without ROS | Distros | Messages | Actions | Verdict |
|---|---|---|---|---|---|---|---|
| **`ros2-client`** (Atostek) on **RustDDS** | 0.12.1 (2026-10-26); RustDDS 0.15.1 | Apache-2.0 (Cargo.toml; the README carries the header; no root LICENSE file) | **Yes**: "does not link to rcl, rclcpp, or any non-Rust DDS library" | Humble to Lyrical by feature; Jazzy default | Serde structs; `.msg` generation "experimental" | Yes ("async required") | **Recommended.** Two backends: DDS interoperates with `rmw_fastrtps` and `rmw_cyclonedds`, Zenoh mirrors `rmw_zenoh`. CI on Ubuntu and macOS; Windows **UNVERIFIED** |
| `hiroz` (ZettaScale, formerly `ros-z`) | 0.2.1 (2026-10-24) | Apache-2.0 OR MIT | Yes: "ROS 2 without ROS" | Humble to Rolling; interop CI against ROS images | Build-time codegen with RIHS01 hashes; bundles common packages, not `vision_msgs` | **UNVERIFIED** | Alternate; Zenoh only; its README calls it "experimental software" |
| `roslibrust` | 0.26.0 (2026-09-25) | MIT | Yes | rosbridge on Humble to Rolling; native through `hiroz` on Kilted and later | Build-time codegen from vendored `.msg` | **No** action trait | Not for S21, which needs actions |
| `r2r` | 0.9.7 (2026-09-13) | MIT AND Apache-2.0 | **No**: panics "Source your ROS!"; needs libclang | Foxy to Lyrical | bindgen over the sourced ROS tree | Yes | Mature; fails the CI constraint |
| `rclrs` (ros2-rust) | 0.8.0 (2026-09-23) | Apache-2.0 | **No**: links `rcl`, `rmw`; colcon plugins | Humble, Jazzy, Kilted, Lyrical, Rolling | `rosidl_generator_rs` in the ROS build | Yes (async, 0.6.0) | The official client; fails the CI constraint; a later optional backend for users with ROS |
| `zenoh` + `cdr-encoding` | 1.10.2; 0.11.0 | EPL-2.0 OR Apache-2.0; Apache-2.0 | Yes | Speaks `rmw_zenoh` only if the key expressions, liveliness tokens, attachments and type hashes are reimplemented | Hand-written | Built from services and topics | Not worth reimplementing what `ros2-client` already has |
| rosbridge (server) | rosbridge_server 4.2.1 | BSD-3-Clause | The client needs nothing; the robot runs the node | Humble to Rolling | JSON | `send_action_goal`, `cancel_action_goal` | A fallback for robots that already run rosbridge; no `auth` operation in the ROS 2 protocol document |
| `safe_drive` (tier4) | 0.4.3 (2024-08-15) | Apache-2.0 text | No | Humble, Jazzy | — | — | Stale |

**Distributions.** The default RMW is still `rmw_fastrtps_cpp` on every distribution,
Lyrical included; `rmw_zenoh_cpp` is Tier 1 on Kilted and Lyrical and the default nowhere
([REP 2000](https://reps.openrobotics.org/rep-2000/); Lyrical's supported-platforms page).
Kilted reaches end of life this month. The bridge therefore speaks DDS by default, targets
Jazzy (LTS to May 2029) and Lyrical (LTS to May 2031), and enables the Zenoh backend for
robots that run `rmw_zenoh`.

### 5.2 Recommendation: `adk-ros2` on `ros2-client`

- **A new crate, not an `adk-vision` feature.** The bridge carries robot-side concerns —
  actions, motion requests, the grant, a transport — that are not perception, and
  `adk-vision`'s dependency rule (it depends on core crates, none on it) stays clean:
  `adk-ros2` depends on `adk-vision`, `adk-core` and, by feature, `adk-kernel`.
- **One transport trait, `RosTransport`, with `ros2-client` as its implementation.** `hiroz`
  and `rclrs` can implement it later without touching the bridge's logic.
- **Vendored messages, CDR by serde.** The bridge needs few types: `std_msgs/Header`,
  `builtin_interfaces/Time`, `geometry_msgs` (`Pose`, `PoseStamped`, `Vector3`,
  `PoseWithCovariance`), `sensor_msgs` (`CameraInfo`, `Image`), `vision_msgs`
  (`Detection2D[Array]`, `Detection3D[Array]`, `BoundingBox2D/3D`, `ObjectHypothesis[WithPose]`,
  `VisionInfo`, `LabelInfo`), `action_msgs`, and `control_msgs/FollowJointTrajectory` for
  forwarding. They are hand-written serde structs with their RIHS01 type hashes, all from
  Apache-2.0 (`vision_msgs`, `common_interfaces`) and BSD-3-Clause (`control_msgs`) packages,
  with attribution. `vision_msgs` definitions are identical on the Humble, Jazzy, Kilted and
  Rolling branches, so one set serves every target.
- **Type hashes are tested.** On Zenoh, "two entities match only if name AND type AND hash
  agree", and "if type hashes differ… nodes silently drop messages" (`ros2-client` ADR-0007;
  `hiroz` documentation). Each vendored type's RIHS01 hash is a golden value recorded from
  `ros2 interface` in the nightly container and asserted in the PR tier.

### 5.3 Detections out

The bridge is an `ObservationSink` ([`stream/pipeline.rs:189`](../../adk-vision/src/stream/pipeline.rs)):
each processed frame, already redacted, with its regions in source pixels and its anonymous
tracks, becomes a message. Regions leave; pixels do not.

| adk-vision | ROS 2 | Note |
|---|---|---|
| `Observation.frame.captured_at` | `header.stamp` | `CameraInfo`: the stamp "should be acquisition time of image" |
| The source's configured optical frame | `header.frame_id` | "should be optical frame of camera", +x right, +y down, +z into the image (`sensor_msgs/CameraInfo`) |
| `Region` with `Shape::Box` | `Detection2D.bbox` (`BoundingBox2D`: centre `Pose2D` and size in pixels) | Centre = box midpoint minus 0.5 px (pixel-centre convention); `theta` 0 |
| `label`, `score` | `results[].hypothesis.class_id`, `.score` | `class_id` is a string in `ObjectHypothesis`, a `uint16` in `LabelInfo`'s `VisionClass` |
| `track` and source session | `Detection2D.id` | "ID used for consistency across multiple detection messages"; `"<session>/<track>"` |
| `RegionOrigin` (backend, manifest) | `VisionInfo` (`method`, `database_location`, `database_version`), latched | The transform chain stays in adk-vision; every published box is already in source pixels |
| The frame's digest and artifact | `adk_vision_msgs/FrameRef { header, digest, artifact }` with the same header | `vision_msgs` "decouple[s] source data from the detection" and tells consumers to match by header, so the digest travels beside the detections, not inside them |
| `Region3` with `Shape3::Box` | `Detection3D.bbox` (`BoundingBox3D`: centre `Pose`, size in metres) in the optical frame | — |
| `Keypoints` | `adk_vision_msgs/Keypoints2DArray` | `vision_msgs` has no keypoint message; a small Apache-2.0 package, published with the bridge |
| `CameraIntrinsics` | `sensor_msgs/CameraInfo` (`k`, `d`, `p`, `distortion_model`) | "K[0] == 0.0 indicates an uncalibrated camera" |
| `DepthMap` | `sensor_msgs/Image`, `32FC1` metres (REP 118), with `CameraInfo` | Published only when configured |
| Masks, polygons, frames | Not published by default | A frame published to ROS is pixels leaving the process: only a redacted frame, only when configured, recorded as an egress |

**Destinations.** DDS discovery is multicast on the local network; Zenoh connects to a router.
Both are destinations: the bridge declares its DDS domain and interfaces, or its Zenoh router
endpoint, and the configuration names which labels may be published at all.

### 5.4 Requests in: a held governed call

S21: "the robot side asks for an arm motion… the motion request reaches the agent as a
governed tool call and is held". The request is an action goal on the bridge; the bridge turns
it into a `GovernedCall` acting for the robot-side principal and holds it on the governed path:

```mermaid
sequenceDiagram
  autonumber
  participant R as Robot side (ROS 2 node or VLA policy)
  participant B as adk-ros2 bridge
  participant G as authorize_tool_call
  participant P as Person (decision card)
  participant A as Agent (optional verifier)
  participant C as Arm controller
  R->>B: action goal: target, limits, frame_id
  B-->>R: ACCEPT_AND_DEFER (status ACCEPTED, not EXECUTING)
  B->>G: GovernedCall robot_motion (NonIdempotent), evidence attached
  G-->>B: Pending
  G->>P: card: latest frame, regions, projected target, limits, digests
  B->>A: wake (≤ 4 KiB, data): request digest, frame refs
  A-->>P: assessment beside the call (cannot decide)
  P->>G: approve, bound to the call's fingerprint
  G-->>B: Execute
  B->>B: MotionGrant from Execute; scene freshness check
  B->>C: FollowJointTrajectory goal (or the robot's own action)
  C-->>B: feedback, result
  B-->>R: goal SUCCEEDED or ABORTED with the reason
```

- **Accept and defer.** ROS 2's `Send Goal` "is expected to return quickly"
  ([ROS 2 actions design](https://design.ros2.org/articles/actions.html)), and `rclcpp_action`
  offers `ACCEPT_AND_DEFER`: "the server accepts the goal, and is going to execute it later"
  ([`server.hpp`](https://github.com/ros2/rclcpp/blob/rolling/rclcpp_action/include/rclcpp_action/server.hpp)).
  The goal waits in `ACCEPTED` while the call is held. Feedback is only published while
  executing, so the card, not the action, shows that a person is deciding. A denial or an
  expired approval ends the goal `ABORTED` with the reason; a robot-side cancel abandons the
  held call through `ToolGate::with_abandon_signal`.
- **The call.** `robot_motion` is a `Tool` with `ToolEffect::NonIdempotent`. Its canonical
  arguments are the request's digest, the target (`Pose3` with its `frame_id`, or a joint
  trajectory), the limits (speed, acceleration, force, workspace box) and the evidence digests,
  so the existing fingerprint binds a decision to that exact motion; a changed target is a new
  call and is held again, as V-I5 does for images. `Tool::approval_evidence` attaches the
  latest frame with the target projected onto it through the camera's intrinsics, the regions
  and the depth map ([`tool.rs:437`](../../adk-core/src/tool.rs)).
- **Never waivable.** The harness gains a `ToolKind::Motion` row like `ComputerUse`: denied in
  ReadOnly and Plan, `AskNonWaivable` in Ask and Auto, and outside `rule_eligible`
  ([`policy.rs:198-211`, `:802`](../../adk-harness/src/policy.rs)). No standing rule, mode or
  agent tool turns a motion into an automatic one.
- **The grant.** The bridge's only path to a command publisher, action client or service that
  moves anything takes a `MotionGrant`. Only the governed path's `Execute` outcome constructs
  one, and a `compile_fail` doctest proves it can be built nowhere else — the pattern
  `ReleasedImage` uses for egress ([`egress/mod.rs:274-282`](../../adk-vision/src/egress/mod.rs)).
  A grant is single-use, names the call's fingerprint, and carries the approved target and
  limits; the bridge sends exactly those.
- **Freshness.** A motion approved against a frame can go stale. The approval carries an expiry
  that denies on lapse ([`ExpiryAction::Deny`, `approval.rs:198`](../../adk-core/src/approval.rs)),
  and before sending, the bridge checks that its latest frame is within a configured age of the
  evidence frame and that the target's region is still detected within tolerance; otherwise the
  call is held again with the new frame. This is spec Q8's rule — ground again and check against
  the previewed target before running — applied to motion.
- **Kill switch.** A freeze ([`governance.rs:389`](../../adk-core/src/governance.rs)) cancels
  every goal the bridge has in flight (`CancelGoal`) and refuses new grants, before reasoning
  stops — `CancellationBridge`'s order for desktops: revoke authority, then stop reasoning.
- **Requests are data.** A goal's fields reach the card and the agent as data, never as
  instructions (V-I16). The card names the requesting node, and an unauthenticated ROS graph —
  no SROS2 — is labelled as such.

### 5.5 Why nothing moves without the governed path

"Nothing moves" is a claim about the bridge, not about the robot. The robot's own controllers,
teach pendant, safety-rated stops and ISO 10218 functions move it, and stop it, without asking
adk-rust; N8 keeps it that way. What the bridge guarantees, as invariants for spec §9.2:

| # | Invariant | Mechanism and test |
|---|---|---|
| V-I18 | The bridge sends a motion command only with a `MotionGrant`, and only `ToolAuthorization::Execute` constructs one | A private constructor; `compile_fail` doctest; S21 asserts the arm received zero goals before approval |
| V-I19 | A motion call is `NonIdempotent`, of a kind no standing rule covers and no mode allows without a person | `ToolKind::Motion` in the mode table; harness table tests |
| V-I20 | A decision binds to the exact motion: target, limits, frame, request digest and evidence digests | The fingerprint over canonical arguments; S21 changes the target and sees the call held again |
| V-I21 | A grant is single-use and fresh: an expired approval denies, and a stale scene holds again | Approval expiry; the freshness check; S21 replays a grant and is refused |
| V-I22 | A freeze cancels every in-flight goal and refuses new grants before reasoning stops | S21 freezes mid-execution and sees `CancelGoal` on the arm |
| V-I23 | Robot-side requests are data, shown with their source; an unauthenticated graph is marked | Card content tests |

### 5.6 The ROS graph's own security

SROS2 authenticates nodes with X.509 certificates ("DDS:Auth:PKI-DH"), controls access per
topic ("DDS:Access:Permission") and encrypts ("DDS:Crypto:AES-GCM-GMAC"), configured through
`ROS_SECURITY_KEYSTORE`, `ROS_SECURITY_ENABLE` and `ROS_SECURITY_STRATEGY=Enforce`
([ROS 2 DDS security design](https://design.ros2.org/articles/ros2_dds_security.html)).
`ros2-client`'s security support is "experimental"; on Zenoh, `zenoh_security_tools` builds
access control and authentication from SROS2 policies, while `rmw_zenoh`'s own design document
leaves security "TBD". Phase D therefore runs the bridge in its own ROS domain or Zenoh
namespace, documents SROS2 as a production requirement, and does not rely on the graph's
authentication for safety: every request is held whoever sent it.

## 6. VLA bridges

### 6.1 What the term should mean here

A vision-language-action model maps images, an instruction and the robot's state to an *action
chunk*: the next 4 to 64 joint or end-effector steps, produced at 5–50 Hz and executed open loop
until the next chunk. Spec §7.19 names a VLA's proposed motion as the example of a request from
the robot side. The rates settle what a bridge can be: no person approves a chunk every 20 ms,
and the only sources that describe a gate between a policy and the hardware put an automatic
check there, not a person. LeRobot's remote inference states it plainly — "Neither transport
delivery nor a successful model call alone authorizes motion", and its "local dispatch and final
eligibility gate" means "network/model progress cannot directly command hardware"
([`remote_inference/README.md`](https://github.com/huggingface/lerobot/tree/main/src/lerobot/remote_inference)).
Research shields do the same with control-barrier functions (AEGIS/VLSA,
[arXiv 2512.11891](https://arxiv.org/abs/2512.11891)). openpi's client executes chunks with no
safety layer.

A **VLA bridge** in ADK-Rust is therefore a client of a policy server the user runs, used in one
of two ways, both on the governed path:

| Mode | What a person approves | Who runs the loop | Fits |
|---|---|---|---|
| **Proposal** | One `robot_motion` call whose evidence is the chunk the policy proposed, drawn as a path on the frame | The bridge sends exactly the approved chunk once, through the grant | Slow, discrete steps: pick this part, place it there |
| **Task grant** | A `policy_run` call: policy, instruction, maximum duration, workspace box (`Shape3::Box` in a named frame), speed and force limits, stop conditions | The robot's own runner (LeRobot rollout, the vendor's controller) runs the closed loop inside the envelope, with its own per-chunk eligibility gate | Dexterous work at the policy's rate |

A task grant is one approval for a bounded run, not a standing rule: it is `NonIdempotent`,
never waivable, single-use and short-lived like a `MotionGrant`, and a freeze cancels the run.
Whether task grants exist in phase D is owner decision O5.

### 6.2 Policies and servers

ADK-Rust does not run VLA weights in process in phase D; the bridge talks to a server. The
policy's licence is still recorded in a manifest entry, as D-7 requires for any model, and the
documentation lists which policies are permissive.

| Policy | Code | Weights (and base model) | Interface | Server | Class |
|---|---|---|---|---|---|
| **SmolVLA** (`lerobot/smolvla_base`), 450M | Apache-2.0 | Apache-2.0; base SmolVLM2-500M, Apache-2.0 | Multi-view images, state, language → chunk of 50 | LeRobot remote inference | Permissive |
| **X-VLA** (`2toINF/X-VLA-Pt`), 0.88B | Apache-2.0 | Apache-2.0; base Florence-2-large, MIT | — | LeRobot | Permissive |
| **RDT-1B** | MIT | MIT; T5-v1.1-XXL and SigLIP, Apache-2.0 | Up to 3 views and language → next 64 actions | Example script | Permissive |
| ACT, Diffusion Policy (LeRobot) | Apache-2.0 (upstreams MIT) | Apache-2.0 | Chunks; Diffusion keeps history | LeRobot | Permissive |
| Octo base/small 1.5 | MIT | MIT | Chunk of 4, JAX | In process | Permissive; unmaintained since 2024 |
| τ0-VLA (`sii-research/tau-0-vla`), 3.0B | Apache-2.0 | "released under the Apache License 2.0"; base Qwen3.5-2B, Apache-2.0 | 30 × 40 chunk | — | Permissive per its card; bundled data CC BY-NC-SA |
| OpenVLA, OpenVLA-OFT | MIT | MIT tag, but "subject to the Llama Community License" (Llama 2 base) | One image and instruction → one 7-DoF delta (OFT: chunks) | FastAPI `POST /act` | UseRestricted at best |
| CogACT | MIT | MIT text; built on OpenVLA's Llama 2 VLM | Chunks of up to 16 | `deploy.py` | UseRestricted at best |
| π0, π0-FAST, π0.5 (openpi) | Apache-2.0 | Gemma Terms of Use (PaliGemma base); checkpoints themselves unstated | Multi-view, state, prompt → chunk of 50, up to 50 Hz | WebSocket with `msgpack_numpy` | UseRestricted |
| GR00T N1.7 | Apache-2.0 | NVIDIA Open Model License: commercial, but "revocable", and rights "automatically terminate" if guardrails are bypassed | Images, language, state → horizon 40 | ZMQ; ONNX and TensorRT export | UseRestricted |
| GR00T N1.5, N1.6 | Apache-2.0 | NVIDIA OneWay Noncommercial License | — | — | NonCommercial |
| Gemini Robotics-ER 2 (preview) | API | Gemini API terms | Points as `[y, x]` on 0–1000; calls "your robot functions" through function calling | Gemini API, Live API | A cloud reasoner, not a policy |

Gemini Robotics-ER already fits the existing contracts: its points are `CoordFrame::Normalized
{ scale: 1000, order: Yx }` regions, and the functions it calls are tools on the governed path,
so a `robot_motion` call it makes is held like any other (owner decision O9 on when to add its
adapter).

### 6.3 The minimal safe interface

```rust
/// A policy server the bridge asks for proposals. Implementations: LeRobot remote inference
/// (Zenoh, MessagePack envelopes) and openpi (WebSocket, msgpack).
#[async_trait]
pub trait PolicyClient: Send + Sync {
    fn descriptor(&self) -> &PolicyDescriptor; // manifest entry, licence, locality
    async fn propose(&self, cx: &VisionCx, observation: &PolicyObservation)
        -> Result<ActionChunk>;
}

/// What a policy proposed. Evidence on a held call, never a command.
pub struct ActionChunk {
    /// Joint names or an end-effector frame, in the robot's convention.
    pub targets: ChunkTargets,
    /// Steps × dimensions, unnormalised.
    pub values: Vec<Vec<f64>>,
    pub rate_hz: f64,
}
```

- **Observations are egress.** A policy server off the machine receives frames: pixels go
  through the egress gate and its privacy step like any remote backend's (`Locality::Remote`);
  a server on loopback is `Loopback`.
- **A chunk is evidence.** It becomes a `robot_motion` call's argument and evidence; only a
  `MotionGrant` sends it.
- **Validation before the card.** Finite values, dimensions matching the robot's description,
  limits within the request's envelope — the checks LeRobot's client makes before a chunk enters
  its queue — run before the call is raised, so a person never approves a malformed chunk.
- **Policy outputs are data.** A policy's language output, like a VLM's, never enters an agent's
  instructions (V-I16).

## 7. Plan

### 7.1 Work breakdown

| Item | Crates and features | Size (engineer-weeks) |
|---|---|---|
| Research spike (this note) | — | 1.5 (done) |
| `CameraIntrinsics` through the chain, `Region3`, `Pose3`, `Shape3`; property tests | `adk-vision` (always built) | 1.5 |
| `DepthEstimator`, `DepthMap`, alignment (scale, scale and shift, plane), `RegionDepth`, depth in `vision_measure`; `depth_conformance_tests!` | `adk-vision` | 1.5 |
| ONNX depth backends: MoGe-2 ViT-S (with the focal and shift recovery its ONNX graph leaves out), Depth Anything V2 Small, DA3METRIC-LARGE opt-in; manifest entries; benchmark rows | `adk-vision` (`onnx`), `adk-bench` | 1.5 |
| Fiducials: AprilTag detection and PnP, `Calibration` from a pose | `adk-vision` (feature `fiducial`) | 0.75 |
| Keypoints: visibility on clamp, skeleton registry, optional box, RF-DETR Keypoint backend, `VisionTask::{Depth, Pose}` | `adk-vision` | 0.75 |
| `adk-ros2`: `RosTransport` over `ros2-client`, vendored messages with golden RIHS01 hashes, detections out as an `ObservationSink`, `CameraInfo`, depth images, `adk_vision_msgs` | `adk-ros2` (new; features `dds` default, `zenoh`) | 2 |
| Requests in: action server with accept-and-defer, `robot_motion`, `MotionGrant`, freshness, freeze-cancel, `ToolKind::Motion`, `robotics.*` audit records | `adk-ros2`, `adk-harness` | 1.5 |
| VLA bridge: `PolicyClient` for LeRobot remote inference and openpi, proposal mode; task grants if O5 says so | `adk-ros2` (feature `policy`) | 1 |
| Scenarios S20 and S21, fixtures, the nightly ROS job, `docs/official_docs/vision/spatial.md` and `robotics.md` | `examples/vision`, CI, docs | 1.5 |
| **Total after the spike** | | **about 12**, against the spec's 8 |

The spec's estimate (9.5 including the spike) assumed one bridge item of 4 weeks. The increase
comes from the camera model and alignment, which the spec did not list, and from vendoring
messages and testing their hashes, which a ROS-free CI requires.

### 7.2 CI without ROS

| Tier | What runs | ROS needed |
|---|---|---|
| PR | `adk-vision` spatial unit, property and conformance tests on synthetic scenes; `adk-ros2` CDR round trips against golden bytes, RIHS01 hashes against golden values, and the bridge against an in-process fake robot built on `ros2-client` (two participants in one process) | No |
| PR (`vision-scenarios`) | S20 geometry gates (a), (d), (e) on rendered scenes; S20 (b) on the recorded calibration scene with weights from the model cache; S21 end to end against the fake robot | No |
| Nightly | A `ros:jazzy` (and `ros:lyrical`) container on Linux: an `rclpy` node subscribes to the detections, checks `frame_id`, stamps and `VisionInfo`, sends a goal and asserts it is held, then approves through the surface API and sees one `FollowJointTrajectory` goal; the same with `RMW_IMPLEMENTATION=rmw_zenoh_cpp`; golden hashes re-recorded and compared | In the container only |
| Feature coverage | `adk-vision` with `fiducial` and with `onnx`; `adk-ros2` with `zenoh` and with `policy` (`scripts/feature-coverage-pairs.txt`) | No |

Two facts to confirm before relying on the PR-tier loopback test: whether RustDDS discovery
works between two participants on a GitHub-hosted runner without multicast (otherwise the
test uses unicast locators, or `ros2-client`'s Zenoh backend in peer mode on loopback), and
whether `ros2-client` builds on Windows (**UNVERIFIED**; until it is, `adk-ros2` is left out
of the Windows job).

### 7.3 Fixtures

| Fixture | For | Content |
|---|---|---|
| Rendered calibration scenes | S20 (a), (d), (e) in the PR tier | A small renderer in the fixture generator (as `adk-vision/tests/fixtures/s5/generate.py` is for S5): AprilTag 36h11 markers of 150 mm at 1.0–3.0 m, a tilted plane, a raised part, exact intrinsics and exact depth |
| Recorded calibration scene | S20 (b) and the depth benchmark | Two printed 150 mm markers on a plain backdrop at tape-measured stations, a checkerboard calibration set, no people and nothing of anyone's surroundings (wave rule: no frames of the owner's surroundings); captured once with a calibrated camera and committed by digest (owner decision O8) |
| Conveyor clip (S9) | S20 (e) and keypoints on parts | S9's fixture; part keypoints need a user fine-tune of RF-DETR Keypoint, as S9 uses a user RF-DETR detector |
| Fake robot | S21 | A `ros2-client` node: publishes `CameraInfo` and frames from the fixture, requests a motion as an action goal, serves `FollowJointTrajectory` and records every goal it receives |
| Golden ROS bytes and hashes | `adk-ros2` | Messages serialised by `rclpy` in the container and their RIHS01 hashes, recorded by a script that runs only in the nightly job |

S21's assertions: detections carry the camera's optical `frame_id` and the capture stamp, and
their `FrameRef` names the frame's digest; the robot's request is held, with the frame, regions
and projected target on the card; the arm receives no goal before approval and exactly the
approved trajectory after it; a changed target is held again; a replayed grant is refused; a
stale scene is held again; a freeze mid-execution cancels the arm's goal; an expired approval
ends the robot's goal `ABORTED`.

### 7.4 Risks

| Risk | Effect | Mitigation |
|---|---|---|
| Zero-shot depth misses 5% on real scenes | S20 (c) fails | Already expected (§3.2): (c) is reported, the gates use PnP and the anchored route |
| The anchored route misses 5% on held-out markers | S20 (b) fails | Its threshold is confirmed by the first benchmark run (D-13); a plane anchor and a second marker add anchors |
| CPU depth latency (ViT-S near 0.2–0.5 s per frame†) | Depth cannot run per frame | On demand only; Core ML execution provider on Apple Silicon |
| `ros2-client` interop gaps with Fast DDS (discovery, QoS, hashes) | Detections or goals not seen | The nightly container job; the `RosTransport` trait to swap clients |
| RIHS01 mismatch | Silent message drops on Zenoh | Golden hashes per type in the PR tier |
| Licence drift: RF-DETR Keypoint is a preview; DA3 cards and tables disagree; MoGe-2's ONNX repository is untagged | A manifest entry's licence becomes wrong | Revisions pinned; licence re-read at each manifest change; the export provenance review of Q6 |
| Readers take "governed" for "safe" | A robot is trusted beyond what the bridge guarantees | §5.5's wording in the documentation; N8 restated; ISO 10218 named as the robot's own obligation |
| An unauthenticated ROS graph | Anyone on the network can ask for motion | Every request is held; the card marks unauthenticated sources; SROS2 documented for production |
| Small maintainer teams (`ros2-client`, `hiroz`) | Fixes lag | Pin versions; the transport trait; `rclrs` as a later backend for users with ROS installed |

## 8. What this changes in spec 3.7

| Spec | Change |
|---|---|
| §5, S20 | The pass conditions of [§3.4](#34-meeting-s20) |
| §7.19 | "Which ROS 2 client crate to use was not researched" becomes `adk-ros2` on `ros2-client` ([§5.2](#52-recommendation-adk-ros2-on-ros2-client)); `DepthEstimator`, `Region3` and the camera model as in [§4](#4-contracts) |
| §9.2 | V-I18 to V-I23 ([§5.5](#55-why-nothing-moves-without-the-governed-path)) |
| §9.3 | `robotics.request.received`, `.held`, `.granted`, `.sent`, `.cancelled`; `vision.depth` with the depth map's digest |
| §10 | `adk-harness` gains `ToolKind::Motion`; a new crate `adk-ros2` |
| §12 | Phase D about 12 weeks after the spike ([§7.1](#71-work-breakdown)) |

## 9. Owner decisions

| # | Question | Recommendation |
|---|---|---|
| O1 | S20's pass conditions | Replace the single line with [§3.4](#34-meeting-s20)'s five: PnP range gated at 5%, the anchored depth route gated on a held-out marker, zero-shot metric depth reported, normalisation gated, out-of-plane measurement gated |
| O2 | Which depth models enter the built-in manifest | MoGe-2 ViT-S and Depth Anything V2 Small; DA3METRIC-LARGE (669 MB fp16) as an opt-in entry; the default chosen by the benchmark |
| O3 | A new crate or an `adk-vision` feature for the bridge, and its name | A new crate, `adk-ros2`, Experimental, in no umbrella tier at first |
| O4 | The ROS 2 client | `ros2-client` (DDS by default, Zenoh by feature) behind `RosTransport`; `hiroz` and `rclrs` as later implementations |
| O5 | Approval granularity for motion | One approval per motion in phase D; task grants for VLA runs only if the owner wants closed-loop policies in 3.0.0. Spec Q8's batched plans do not extend to motion |
| O6 | VLA bridge scope | Proposal mode with LeRobot remote inference and openpi clients; the documentation names SmolVLA, X-VLA, RDT-1B and ACT as permissive, and π0, OpenVLA and GR00T as restricted |
| O7 | Human pose under N2 and D-12 | Allowed for anonymous safety zones near a robot; never stored with faces; part keypoints through users' fine-tunes |
| O8 | Who records the real calibration scene, and where | A staged neutral backdrop with printed markers and no people, recorded once; rendered scenes for every PR-tier gate |
| O9 | Gemini Robotics-ER 2 as a cloud spatial reasoner | After phase D: it needs no new contract, only an adapter, and it is a preview |
| O10 | ROS distributions | Jazzy and Lyrical (LTS); Humble best effort until its May 2027 end of life; not Kilted (end of life this month) |
| O11 | Frames published to ROS | Never by default; a redacted frame only when configured, recorded as an egress |

## References

All read on 2026-11-23. Hugging Face licence tags were read through the Hub API
(`huggingface.co/api/models/<repo>`), crate versions through the crates.io API.

**Depth**
- Depth Anything V2: repository and licence section <https://github.com/DepthAnything/Depth-Anything-V2>; paper <https://arxiv.org/abs/2406.09414>; cards <https://huggingface.co/depth-anything/Depth-Anything-V2-Small-hf>, <https://huggingface.co/depth-anything/Depth-Anything-V2-Metric-Hypersim-Small>
- Depth Anything V2 ONNX and Core ML: <https://huggingface.co/onnx-community/depth-anything-v2-small>, <https://github.com/fabio-sim/Depth-Anything-ONNX>, <https://huggingface.co/apple/coreml-depth-anything-v2-small>
- Depth Anything 3: repository and model table <https://github.com/ByteDance-Seed/Depth-Anything-3>; paper <https://arxiv.org/abs/2511.10647>; cards <https://huggingface.co/depth-anything/DA3METRIC-LARGE>, <https://huggingface.co/depth-anything/DA3-SMALL>; ONNX <https://huggingface.co/Heliosoph/da3metric-large-onnx>
- MoGe: repository and licence <https://github.com/microsoft/MoGe>; MoGe-2 paper <https://arxiv.org/abs/2507.02546>; weights <https://huggingface.co/Ruicheng/moge-2-vits-normal>; ONNX <https://huggingface.co/Ruicheng/moge-2-vits-normal-onnx>
- Depth Pro: <https://github.com/apple-aiml-research/ml-depth-pro>, <https://huggingface.co/apple/DepthPro>, paper <https://arxiv.org/abs/2410.02073>; Depth Pro 2 <https://arxiv.org/abs/2611.26603>
- Metric3D: <https://github.com/YvanYin/Metric3D>; UniDepth: <https://github.com/lpiccinelli-eth/UniDepth>; Marigold: <https://github.com/prs-eth/Marigold>; Video Depth Anything: <https://github.com/DepthAnything/Video-Depth-Anything>; Prompt Depth Anything: <https://github.com/DepthAnything/PromptDA>; DepthCrafter: <https://github.com/Tencent/DepthCrafter>; DepthART: <https://arxiv.org/abs/2607.17099>
- Jetson AGX Orin depth benchmark (October 2026): <https://arxiv.org/abs/2610.27199>
- Alignment practice: MiDaS <https://arxiv.org/abs/1907.01341>; VI-Depth <https://arxiv.org/abs/2303.12134>
- OpenCV ArUco pose tutorial (source): <https://github.com/opencv/opencv/blob/4.x/doc/tutorials/objdetect/aruco_detection/aruco_detection.markdown>

**Pose and fiducials**
- RF-DETR, including RF-DETR Keypoint and the licence statement: <https://github.com/roboflow/rf-detr>, <https://rfdetr.roboflow.com/latest/>
- MMPose (RTMPose, RTMO, RTMW): <https://github.com/open-mmlab/mmpose>; rtmlib <https://github.com/Tau-J/rtmlib>
- ViTPose: <https://github.com/ViTAE-Transformer/ViTPose>, <https://huggingface.co/usyd-community/vitpose-base-simple>
- MediaPipe Pose Landmarker: <https://github.com/google-ai-edge/mediapipe>, <https://huggingface.co/qualcomm/MediaPipe-Pose-Estimation>
- Sapiens: <https://github.com/facebookresearch/sapiens>; FoundationPose: <https://github.com/NVlabs/FoundationPose>; Ultralytics licence <https://www.ultralytics.com/license>
- AprilTag: <https://github.com/AprilRobotics/apriltag>; crates `kornia-apriltag`, `kornia-3d` <https://github.com/kornia/kornia-rs>; `locus-core`, `sqpnp`, `opencv-ros-camera` on <https://crates.io>

**ROS 2**
- `ros2-client`: <https://github.com/Atostek/ros2-client> (README, ADR-0007); RustDDS <https://github.com/Atostek/RustDDS>
- `hiroz`: <https://github.com/ZettaScaleLabs/hiroz>; `roslibrust`: <https://github.com/RosLibRust/roslibrust>; `r2r`: <https://github.com/sequenceplanner/r2r>; `rclrs`: <https://github.com/ros2-rust/ros2_rust>, release announcement <https://discourse.openrobotics.org/t/announcement-rclrs-0-8-0-release/58347>
- Zenoh: <https://github.com/eclipse-zenoh/zenoh>; `rmw_zenoh` design <https://github.com/ros2/rmw_zenoh/blob/rolling/docs/design.md>
- rosbridge: <https://github.com/RobotWebTools/rosbridge_suite>
- `vision_msgs`: <https://github.com/ros-perception/vision_msgs>; `common_interfaces` (`std_msgs`, `sensor_msgs`): <https://github.com/ros2/common_interfaces>; `control_msgs`: <https://github.com/ros-controls/control_msgs>; `moveit_msgs`: <https://github.com/moveit/moveit_msgs>
- REP 103 <https://reps.openrobotics.org/rep-0103/>, REP 105 <https://reps.openrobotics.org/rep-0105/>, REP 118 (source) <https://github.com/ros-infrastructure/rep/blob/master/rep-0118.rst>, REP 2000 <https://reps.openrobotics.org/rep-2000/>
- ROS 2 actions design <https://design.ros2.org/articles/actions.html>; `rclcpp_action` `GoalResponse` <https://github.com/ros2/rclcpp/blob/rolling/rclcpp_action/include/rclcpp_action/server.hpp>
- ROS 2 DDS security design <https://design.ros2.org/articles/ros2_dds_security.html>

**VLA**
- LeRobot and remote inference: <https://github.com/huggingface/lerobot>, <https://github.com/huggingface/lerobot/tree/main/src/lerobot/remote_inference>; SmolVLA <https://huggingface.co/lerobot/smolvla_base>
- openpi: <https://github.com/Physical-Intelligence/openpi>; π0 paper <https://arxiv.org/abs/2410.24164>; Gemma terms <https://ai.google.dev/gemma/terms>
- OpenVLA <https://github.com/openvla/openvla>; OpenVLA-OFT <https://github.com/moojink/openvla-oft>; CogACT <https://github.com/microsoft/CogACT>
- GR00T: <https://github.com/NVIDIA/Isaac-GR00T>, <https://huggingface.co/nvidia/GR00T-N1.7-3B>
- X-VLA <https://huggingface.co/2toINF/X-VLA-Pt>; RDT-1B <https://huggingface.co/robotics-diffusion-transformer/rdt-1b>; Octo <https://github.com/octo-models/octo>; τ0-VLA <https://huggingface.co/sii-research/tau-0-vla>
- Gemini Robotics-ER: <https://aistudio.google.com/docs/robotics-overview>
- VLSA/AEGIS safety layer <https://arxiv.org/abs/2512.11891>

**Safety standards**
- ISO 10218-2:2025 <https://www.iso.org/standard/73934.html>; revision coverage <https://www.therobotreport.com/iso-10218-industrial-robot-safety-standard-receives-major-overhaul/>†
