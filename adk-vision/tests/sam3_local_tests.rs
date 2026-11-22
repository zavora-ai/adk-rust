//! `sam3-local` without SAM 3's weights: stand-in graphs with the inputs and outputs of the
//! Transformers `Sam3Model` and `Sam3TrackerModel` exports, written here as ONNX, exercise
//! the licence gate, a manifest the user writes, the backends' pre- and post-processing, the
//! coordinate mapping and the conformance suite.
//!
//! The stand-ins compute nothing SAM 3 computes. The concept graphs mark the pixels whose
//! colour the concept names (`red`, `green`, `blue`) and answer a fixed box per colour; the
//! tracker graphs answer each box prompt as a mask filling that box. The real models are
//! exercised by `sam3_local_live_tests` once a person supplies weights and accepts the SAM
//! License.

#![cfg(feature = "sam3-local")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use adk_vision::conformance::{DetectorTarget, Fixture, SegmenterTarget};
use adk_vision::onnx::{OnnxOptions, SAM_LICENCE_ID, Sam3, Sam3Tracker};
use adk_vision::{
    AcceptanceStore, BBox, DetectQuery, Digest, FetchConfig, InMemoryAcceptanceStore,
    LicenceAcceptance, LicenceGate, LicencePolicy, ModelFetcher, ModelManifest, SegmentPrompt,
    Shape, VerifiedModel, VisionCx, VisionError, VisionRuntime,
};

/// Meta's SAM License, pinned to the commit of `facebookresearch/sam3` that published it.
const SAM_LICENSE_URL: &str = "https://github.com/facebookresearch/sam3/blob/a13e358df479a80938c8bfed0e0754bea8fdb277/LICENSE";

/// A minimal ONNX writer: the protobuf fields of `ModelProto` the stand-ins need.
mod onnx {
    pub const FLOAT: i64 = 1;
    pub const INT64: i64 = 7;

    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8 & 0x7f) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn tag(out: &mut Vec<u8>, field: u32, wire: u32) {
        varint(out, u64::from(field << 3 | wire));
    }

    fn int(out: &mut Vec<u8>, field: u32, v: i64) {
        tag(out, field, 0);
        varint(out, v as u64);
    }

    fn bytes(out: &mut Vec<u8>, field: u32, b: &[u8]) {
        tag(out, field, 2);
        varint(out, b.len() as u64);
        out.extend_from_slice(b);
    }

    /// An attribute value.
    pub enum Attr {
        Int(i64),
        Str(&'static str),
    }

    /// A graph under construction.
    #[derive(Default)]
    pub struct Graph {
        nodes: Vec<Vec<u8>>,
        initializers: Vec<Vec<u8>>,
        inputs: Vec<Vec<u8>>,
        outputs: Vec<Vec<u8>>,
    }

    /// `dims` with `-1` for a dimension named at run time.
    fn value_info(name: &str, elem: i64, dims: &[i64]) -> Vec<u8> {
        let mut shape = Vec::new();
        for (i, &d) in dims.iter().enumerate() {
            let mut dim = Vec::new();
            if d < 0 {
                bytes(&mut dim, 2, format!("d{i}").as_bytes());
            } else {
                int(&mut dim, 1, d);
            }
            bytes(&mut shape, 1, &dim);
        }
        let mut tensor = Vec::new();
        int(&mut tensor, 1, elem);
        bytes(&mut tensor, 2, &shape);
        let mut ty = Vec::new();
        bytes(&mut ty, 1, &tensor);
        let mut info = Vec::new();
        bytes(&mut info, 1, name.as_bytes());
        bytes(&mut info, 2, &ty);
        info
    }

    impl Graph {
        pub fn input(&mut self, name: &str, elem: i64, dims: &[i64]) {
            self.inputs.push(value_info(name, elem, dims));
        }

        pub fn output(&mut self, name: &str, elem: i64, dims: &[i64]) {
            self.outputs.push(value_info(name, elem, dims));
        }

        fn initializer(&mut self, name: &str, dims: &[i64], elem: i64, raw: Vec<u8>) {
            let mut t = Vec::new();
            for &d in dims {
                int(&mut t, 1, d);
            }
            int(&mut t, 2, elem);
            bytes(&mut t, 8, name.as_bytes());
            bytes(&mut t, 9, &raw);
            self.initializers.push(t);
        }

        pub fn floats(&mut self, name: &str, dims: &[i64], values: &[f32]) {
            self.initializer(
                name,
                dims,
                FLOAT,
                values.iter().flat_map(|v| v.to_le_bytes()).collect(),
            );
        }

        pub fn ints(&mut self, name: &str, dims: &[i64], values: &[i64]) {
            self.initializer(
                name,
                dims,
                INT64,
                values.iter().flat_map(|v| v.to_le_bytes()).collect(),
            );
        }

        pub fn node(&mut self, op: &str, inputs: &[&str], output: &str, attrs: &[(&str, Attr)]) {
            let mut n = Vec::new();
            for input in inputs {
                bytes(&mut n, 1, input.as_bytes());
            }
            bytes(&mut n, 2, output.as_bytes());
            bytes(&mut n, 4, op.as_bytes());
            for (name, value) in attrs {
                let mut a = Vec::new();
                bytes(&mut a, 1, name.as_bytes());
                match value {
                    Attr::Int(i) => {
                        int(&mut a, 3, *i);
                        int(&mut a, 20, 2);
                    }
                    Attr::Str(s) => {
                        bytes(&mut a, 4, s.as_bytes());
                        int(&mut a, 20, 3);
                    }
                }
                bytes(&mut n, 5, &a);
            }
            self.nodes.push(n);
        }

        /// The serialised `ModelProto`, IR version 8, opset 17.
        pub fn model(&self) -> Vec<u8> {
            let mut g = Vec::new();
            for n in &self.nodes {
                bytes(&mut g, 1, n);
            }
            bytes(&mut g, 2, b"stand-in");
            for t in &self.initializers {
                bytes(&mut g, 5, t);
            }
            for i in &self.inputs {
                bytes(&mut g, 11, i);
            }
            for o in &self.outputs {
                bytes(&mut g, 12, o);
            }
            let mut opset = Vec::new();
            bytes(&mut opset, 1, b"");
            int(&mut opset, 2, 17);
            let mut m = Vec::new();
            int(&mut m, 1, 8);
            bytes(&mut m, 2, b"adk-vision sam3 stand-in");
            bytes(&mut m, 7, &g);
            bytes(&mut m, 8, &opset);
            m
        }
    }
}

use onnx::{Attr, FLOAT, Graph, INT64};

const MASK: i64 = 288;
/// The stand-in boxes per colour, in `0..1` corners: the conformance fixture's shapes on its
/// 640×480 image (red and blue), and one more for green.
const BOXES: [[f32; 4]; 3] = [
    [100.0 / 640.0, 80.0 / 480.0, 260.0 / 640.0, 200.0 / 480.0],
    [0.5, 0.5, 0.6, 0.6],
    [380.0 / 640.0, 260.0 / 480.0, 560.0 / 640.0, 420.0 / 480.0],
];

/// An image encoder that downsamples the image to the three feature-map sizes.
fn encoder(names: [&str; 3]) -> Vec<u8> {
    let mut g = Graph::default();
    g.input("pixel_values", FLOAT, &[1, 3, 1008, 1008]);
    for (name, side) in names.into_iter().zip([MASK, MASK / 2, MASK / 4]) {
        let sizes = format!("{name}.sizes");
        g.ints(&sizes, &[4], &[1, 3, side, side]);
        g.node("Resize", &["pixel_values", "", "", &sizes], name, &[("mode", Attr::Str("linear"))]);
        g.output(name, FLOAT, &[1, 3, side, side]);
    }
    g.model()
}

/// Token ids to one-hot colours: `red` → R, `green` → G, `blue` → B, anything else → 0.
fn text_encoder() -> Vec<u8> {
    let mut g = Graph::default();
    g.input("input_ids", INT64, &[1, 32]);
    g.input("attention_mask", INT64, &[1, 32]);
    #[rustfmt::skip]
    let table = [
        0.0, 0.0, 0.0, // <|startoftext|>
        0.0, 0.0, 0.0, // <|endoftext|>
        1.0, 0.0, 0.0, // red
        0.0, 1.0, 0.0, // green
        0.0, 0.0, 1.0, // blue
    ];
    g.floats("table", &[5, 3], &table);
    g.ints("axis2", &[1], &[2]);
    g.node("Gather", &["table", "input_ids"], "colours", &[("axis", Attr::Int(0))]);
    g.node("Cast", &["attention_mask"], "mask_f", &[("to", Attr::Int(FLOAT))]);
    g.node("Unsqueeze", &["mask_f", "axis2"], "mask_3", &[]);
    g.node("Mul", &["colours", "mask_3"], "text_features", &[]);
    g.output("text_features", FLOAT, &[1, 32, 3]);
    g.model()
}

/// One query: a mask logit of `2·(colour·pixel) − Σpixel − 1`, positive on pixels of the
/// concept's colour in `[-1, 1]` RGB; the colour's box; a presence logit of +4 for a known
/// colour and −4 for anything else.
fn concept_decoder() -> Vec<u8> {
    let mut g = Graph::default();
    for (i, side) in [MASK, MASK / 2, MASK / 4].into_iter().enumerate() {
        g.input(&format!("fpn_hidden_state_{i}"), FLOAT, &[1, 3, side, side]);
    }
    g.input("text_features", FLOAT, &[1, 32, 3]);
    g.input("attention_mask", INT64, &[1, 32]);
    g.ints("axis1", &[1], &[1]);
    g.ints("colour_shape", &[4], &[1, 3, 1, 1]);
    g.ints("box_shape", &[3], &[1, 1, 4]);
    g.floats("boxes", &[3, 4], &BOXES.concat());
    for (name, v) in [("zero", 0.0), ("one", 1.0), ("two", 2.0), ("four", 4.0), ("eight", 8.0)] {
        g.floats(name, &[1], &[v]);
    }
    let keep = |k| [("keepdims", Attr::Int(k))];
    g.node("ReduceSum", &["text_features", "axis1"], "colour", &keep(0));
    g.node("Reshape", &["colour", "colour_shape"], "colour_4d", &[]);
    g.node("Mul", &["fpn_hidden_state_0", "colour_4d"], "weighted", &[]);
    g.node("ReduceSum", &["weighted", "axis1"], "dot", &keep(1));
    g.node("ReduceSum", &["fpn_hidden_state_0", "axis1"], "total", &keep(1));
    g.node("Mul", &["dot", "two"], "dot2", &[]);
    g.node("Sub", &["dot2", "total"], "margin", &[]);
    g.node("Sub", &["margin", "one"], "pred_masks", &[]);
    g.node("MatMul", &["colour", "boxes"], "box", &[]);
    g.node("Reshape", &["box", "box_shape"], "pred_boxes", &[]);
    g.node("ReduceSum", &["colour", "axis1"], "known", &keep(1));
    // The other inputs, consumed as zero.
    g.node("ReduceSum", &["fpn_hidden_state_1"], "z1", &keep(0));
    g.node("ReduceSum", &["fpn_hidden_state_2"], "z2", &keep(0));
    g.node("Cast", &["attention_mask"], "mask_f", &[("to", Attr::Int(FLOAT))]);
    g.node("ReduceSum", &["mask_f"], "z3", &keep(0));
    g.node("Sum", &["z1", "z2", "z3"], "zs", &[]);
    g.node("Mul", &["zs", "zero"], "nothing", &[]);
    g.node("Mul", &["known", "eight"], "known8", &[]);
    g.node("Sub", &["known8", "four"], "presence", &[]);
    g.node("Add", &["presence", "nothing"], "presence_logits", &[]);
    g.node("Mul", &["known", "zero"], "known0", &[]);
    g.node("Add", &["known0", "four"], "pred_logits", &[]);
    g.output("pred_masks", FLOAT, &[1, 1, MASK, MASK]);
    g.output("pred_boxes", FLOAT, &[1, 1, 4]);
    g.output("pred_logits", FLOAT, &[1, 1]);
    g.output("presence_logits", FLOAT, &[1, 1]);
    g.model()
}

/// SAM 2's decoder contract answering one box prompt with three copies of a mask filling the
/// box: a logit of the distance inside the box's nearest edge, on 288×288 cell centres.
fn tracker_decoder() -> Vec<u8> {
    let mut g = Graph::default();
    g.input("input_points", FLOAT, &[1, 1, -1, 2]);
    g.input("input_labels", INT64, &[1, 1, -1]);
    g.input("input_boxes", FLOAT, &[1, -1, 4]);
    for (i, side) in [MASK, MASK / 2, MASK / 4].into_iter().enumerate() {
        g.input(&format!("image_embeddings.{i}"), FLOAT, &[1, 3, side, side]);
    }
    let cell = 1008.0 / MASK as f32;
    let centres: Vec<f32> = (0..MASK).map(|i| (i as f32 + 0.5) * cell).collect();
    g.floats("gx", &[1, MASK], &centres);
    g.floats("gy", &[MASK, 1], &centres);
    g.ints("flat", &[1], &[4]);
    for i in 0..4 {
        g.ints(&format!("s{i}"), &[1], &[i]);
        g.ints(&format!("e{i}"), &[1], &[i + 1]);
    }
    g.ints("mask_shape", &[5], &[1, 1, 3, MASK, MASK]);
    g.floats("iou", &[1, 1, 3], &[0.9, 0.6, 0.3]);
    g.floats("present", &[1, 1, 1], &[5.0]);
    g.floats("zero", &[1], &[0.0]);
    g.node("Reshape", &["input_boxes", "flat"], "b", &[]);
    for (i, name) in ["x0", "y0", "x1", "y1"].into_iter().enumerate() {
        g.node("Slice", &["b", &format!("s{i}"), &format!("e{i}")], name, &[]);
    }
    g.node("Sub", &["gx", "x0"], "left", &[]);
    g.node("Sub", &["x1", "gx"], "right", &[]);
    g.node("Sub", &["gy", "y0"], "top", &[]);
    g.node("Sub", &["y1", "gy"], "bottom", &[]);
    g.node("Min", &["left", "right", "top", "bottom"], "inside", &[]);
    g.node("Expand", &["inside", "mask_shape"], "pred_masks", &[]);
    g.node("Identity", &["iou"], "iou_scores", &[]);
    let keep0 = [("keepdims", Attr::Int(0))];
    g.node("ReduceSum", &["input_points"], "z1", &keep0);
    g.node("Cast", &["input_labels"], "labels_f", &[("to", Attr::Int(FLOAT))]);
    g.node("ReduceSum", &["labels_f"], "z2", &keep0);
    for i in 0..3 {
        g.node("ReduceSum", &[&format!("image_embeddings.{i}")], &format!("e{i}sum"), &keep0);
    }
    g.node("Sum", &["z1", "z2", "e0sum", "e1sum", "e2sum"], "zs", &[]);
    g.node("Mul", &["zs", "zero"], "nothing", &[]);
    g.node("Add", &["present", "nothing"], "object_score_logits", &[]);
    g.output("iou_scores", FLOAT, &[1, 1, 3]);
    g.output("pred_masks", FLOAT, &[1, 1, 3, MASK, MASK]);
    g.output("object_score_logits", FLOAT, &[1, 1, 1]);
    g.model()
}

/// A word-level `tokenizer.json` with CLIP's special tokens and the three colours.
fn tokenizer() -> Vec<u8> {
    use tokenizers::Tokenizer;
    use tokenizers::models::wordlevel::WordLevel;
    use tokenizers::pre_tokenizers::whitespace::Whitespace;
    let vocab = ["<|startoftext|>", "<|endoftext|>", "red", "green", "blue"]
        .into_iter()
        .enumerate()
        .map(|(i, t)| (t.to_string(), i as u32))
        .collect();
    let model =
        WordLevel::builder().vocab(vocab).unk_token("<|endoftext|>".into()).build().unwrap();
    let mut tokenizer = Tokenizer::new(model);
    tokenizer.with_pre_tokenizer(Some(Whitespace {}));
    tokenizer.to_string(false).unwrap().into_bytes()
}

/// Writes the stand-in files once per directory; each write lands by rename, so test
/// processes running at once never read a half-written file.
fn files() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("sam3-stand-in");
        std::fs::create_dir_all(&dir).unwrap();
        let files: [(&str, Vec<u8>); 6] = [
            (
                "vision_encoder.onnx",
                encoder(["fpn_hidden_state_0", "fpn_hidden_state_1", "fpn_hidden_state_2"]),
            ),
            ("text_encoder.onnx", text_encoder()),
            ("prompt_decoder.onnx", concept_decoder()),
            ("tokenizer.json", tokenizer()),
            (
                "tracker/vision_encoder.onnx",
                encoder(["image_embeddings.0", "image_embeddings.1", "image_embeddings.2"]),
            ),
            ("tracker/prompt_encoder_mask_decoder.onnx", tracker_decoder()),
        ];
        for (path, bytes) in files {
            let target = dir.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            let staging = target.with_extension(format!("{}.tmp", std::process::id()));
            std::fs::write(&staging, bytes).unwrap();
            std::fs::rename(&staging, &target).unwrap();
        }
        dir
    })
}

/// The manifest a user writes for their own export: local files, each pinned by SHA-256,
/// under Meta's SAM License.
fn manifest(id: &str, paths: &[&str], class: &str) -> ModelManifest {
    let dir = files();
    let files: Vec<serde_json::Value> = paths
        .iter()
        .map(|path| {
            let bytes = std::fs::read(dir.join(path)).unwrap();
            serde_json::json!({
                "path": path,
                "sha256": Digest::of(&bytes).to_string(),
                "size": bytes.len(),
            })
        })
        .collect();
    ModelManifest::from_json(
        &serde_json::json!({
            "id": id,
            "task": "segment",
            "source": {"kind": "local", "dir": dir},
            "files": files,
            "licence": {"id": SAM_LICENCE_ID, "url": SAM_LICENSE_URL, "class": class},
            "runtime": {"kind": "ort", "opset": 17}
        })
        .to_string(),
    )
    .unwrap()
}

const CONCEPT_FILES: [&str; 4] =
    ["vision_encoder.onnx", "text_encoder.onnx", "prompt_decoder.onnx", "tokenizer.json"];
const TRACKER_FILES: [&str; 2] =
    ["tracker/vision_encoder.onnx", "tracker/prompt_encoder_mask_decoder.onnx"];

/// Verifies `manifest` after recording a test principal's acceptance in a fresh store.
fn accepted(manifest: &ModelManifest) -> VerifiedModel {
    let store = InMemoryAcceptanceStore::new();
    store.record(LicenceAcceptance::of(manifest, "reviewer@example.com")).unwrap();
    let policy = LicencePolicy::new();
    VerifiedModel::check_dir(manifest, files(), &LicenceGate::new(&policy, &store)).unwrap()
}

fn sam3() -> Arc<Sam3> {
    static SAM3: OnceLock<Arc<Sam3>> = OnceLock::new();
    Arc::clone(SAM3.get_or_init(|| {
        let model = accepted(&manifest("sam3-stand-in", &CONCEPT_FILES, "useRestricted"));
        Arc::new(Sam3::new(&model, &OnnxOptions::default()).unwrap())
    }))
}

fn tracker() -> Sam3Tracker {
    let model = accepted(&manifest("sam3-tracker-stand-in", &TRACKER_FILES, "useRestricted"));
    Sam3Tracker::new(&model, &OnnxOptions::default()).unwrap()
}

fn sam3_with_tracker() -> Arc<Sam3> {
    static BOTH: OnceLock<Arc<Sam3>> = OnceLock::new();
    Arc::clone(BOTH.get_or_init(|| {
        let model = accepted(&manifest("sam3-stand-in", &CONCEPT_FILES, "useRestricted"));
        let sam3 = Sam3::new(&model, &OnnxOptions::default()).unwrap();
        Arc::new(sam3.with_tracker(tracker()).unwrap())
    }))
}

/// The conformance shapes, within 4 px: masks come from a 288×288 grid over a 1008×1008
/// stretch of the 640×480 image.
fn shapes() -> Fixture {
    let mut fixture = Fixture::shapes();
    fixture.tolerance_px = 4.0;
    fixture
}

/// The shapes with each object's box as its prompt, for the tracker.
fn boxed_shapes() -> Fixture {
    let mut fixture = shapes();
    fixture.prompts = fixture.objects.iter().map(|o| SegmentPrompt::Box { bbox: o.bbox }).collect();
    fixture
}

fn within(got: BBox, want: BBox, tolerance: f32) -> bool {
    [got.x0 - want.x0, got.y0 - want.y0, got.x1 - want.x1, got.y1 - want.y1]
        .iter()
        .all(|d| d.abs() <= tolerance)
}

#[test]
fn verification_refuses_until_a_person_accepts_the_sam_licence() {
    let manifest = manifest("sam3-stand-in", &CONCEPT_FILES, "useRestricted");
    let store = Arc::new(InMemoryAcceptanceStore::new());
    let fetcher =
        ModelFetcher::new(FetchConfig::from_lookup(|_| None).with_offline(true), store.clone());
    let refused = fetcher.fetch_blocking(&manifest).unwrap_err();
    assert!(
        matches!(&refused, VisionError::LicenceAcceptanceRequired { model, licence_url }
            if model == "sam3-stand-in" && licence_url == SAM_LICENSE_URL),
        "{refused}"
    );
    let message = refused.to_string();
    assert!(message.contains("licence acceptance required"), "{message}");
    assert!(message.contains(SAM_LICENSE_URL), "{message}");
    assert!(message.contains("adk-rust vision models accept sam3-stand-in"), "{message}");

    // An acceptance of another licence URL does not cover it.
    let mut stale = LicenceAcceptance::of(&manifest, "reviewer@example.com");
    stale.licence_url = "https://github.com/facebookresearch/sam3/blob/main/LICENSE".into();
    store.record(stale).unwrap();
    assert!(matches!(
        fetcher.fetch_blocking(&manifest),
        Err(VisionError::LicenceAcceptanceRequired { .. })
    ));

    store.record(LicenceAcceptance::of(&manifest, "reviewer@example.com")).unwrap();
    let model = fetcher.fetch_blocking(&manifest).unwrap();
    assert_eq!(model.files().count(), CONCEPT_FILES.len());
    Sam3::new(&model, &OnnxOptions::default()).unwrap();
}

#[test]
fn a_manifest_that_classes_sam3_weights_otherwise_is_refused() {
    let policy = LicencePolicy::new();
    let store = InMemoryAcceptanceStore::new();
    let gate = LicenceGate::new(&policy, &store);
    // Classed permissive, the weights would pass the gate without anyone accepting.
    let concepts = manifest("sam3-stand-in", &CONCEPT_FILES, "permissive");
    let model = VerifiedModel::check_dir(&concepts, files(), &gate).unwrap();
    let err = Sam3::new(&model, &OnnxOptions::default()).unwrap_err();
    assert!(matches!(err, VisionError::Manifest { .. }), "{err}");
    assert!(err.to_string().contains("SAM License"), "{err}");
    let tracking = manifest("sam3-tracker-stand-in", &TRACKER_FILES, "permissive");
    let model = VerifiedModel::check_dir(&tracking, files(), &gate).unwrap();
    let err = Sam3Tracker::new(&model, &OnnxOptions::default()).unwrap_err();
    assert!(err.to_string().contains("SAM License"), "{err}");
}

#[test]
fn a_manifest_missing_a_graph_or_the_tokenizer_is_refused_by_name() {
    let without_tokenizer = manifest(
        "sam3-stand-in",
        &["vision_encoder.onnx", "text_encoder.onnx", "prompt_decoder.onnx"],
        "useRestricted",
    );
    let err = Sam3::new(&accepted(&without_tokenizer), &OnnxOptions::default()).unwrap_err();
    assert!(err.to_string().contains("tokenizer.json"), "{err}");
    let without_text = manifest(
        "sam3-stand-in",
        &["vision_encoder.onnx", "prompt_decoder.onnx", "tokenizer.json"],
        "useRestricted",
    );
    let err = Sam3::new(&accepted(&without_text), &OnnxOptions::default()).unwrap_err();
    assert!(err.to_string().contains("`text`"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concepts_segment_to_masks_and_detect_to_boxes_in_source_pixels() {
    let fixture = shapes();
    let cx = VisionCx::new();
    let segmenter = VisionRuntime::builder().segmenter(sam3()).build().unwrap();
    let found = segmenter.segment(&cx, &fixture.image, &fixture.prompts).await.unwrap().value;
    assert_eq!(found.len(), 2, "{found:?}");
    for (region, object) in found.iter().zip(&fixture.objects) {
        assert_eq!(region.label, object.label);
        let Shape::Mask(mask) = &region.shape else { panic!("{:?}", region.shape) };
        assert_eq!((mask.width, mask.height), (640, 480));
        let bbox = mask.bbox().unwrap();
        assert!(within(bbox, object.bbox, 4.0), "{:?}: {bbox:?}", object.label);
        assert!(region.score.unwrap() > 0.9);
    }

    let detector = VisionRuntime::builder().detector(sam3()).build().unwrap();
    let query = DetectQuery::Classes(vec!["blue".into(), "red".into()]);
    let found = detector.detect(&cx, &fixture.image, &query).await.unwrap().value;
    let boxes: Vec<(Option<&str>, BBox)> =
        found.iter().map(|r| (r.label.as_deref(), r.shape.bbox().unwrap())).collect();
    assert_eq!(boxes.len(), 2, "{boxes:?}");
    assert_eq!(boxes[0].0, Some("blue"));
    assert!(within(boxes[0].1, fixture.objects[1].bbox, 0.01), "{boxes:?}");
    assert_eq!(boxes[1].0, Some("red"));
    assert!(within(boxes[1].1, fixture.objects[0].bbox, 0.01), "{boxes:?}");
    assert!(found.iter().all(|r| matches!(r.shape, Shape::Box(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_absent_concept_finds_nothing_and_a_lower_threshold_does_not_invent_one() {
    let fixture = shapes();
    let cx = VisionCx::new();
    let model = accepted(&manifest("sam3-stand-in", &CONCEPT_FILES, "useRestricted"));
    let eager = Sam3::new(&model, &OnnxOptions::default()).unwrap().with_threshold(0.1);
    for backend in [sam3(), Arc::new(eager)] {
        let runtime = VisionRuntime::builder().segmenter(backend).build().unwrap();
        let prompts = [SegmentPrompt::Concept { text: "zebra".into() }];
        let found = runtime.segment(&cx, &fixture.image, &prompts).await.unwrap().value;
        assert!(found.is_empty(), "{found:?}");
    }
    // Present in the vocabulary, absent from the image: a box but no mask pixel.
    let runtime = VisionRuntime::builder().segmenter(sam3()).build().unwrap();
    let prompts = [SegmentPrompt::Concept { text: "green".into() }];
    let found = runtime.segment(&cx, &fixture.image, &prompts).await.unwrap().value;
    assert_eq!(found.len(), 1);
    assert!(matches!(found[0].shape, Shape::Box(_)), "{:?}", found[0].shape);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn point_and_box_prompts_need_the_tracker_and_masks_are_refused() {
    let fixture = boxed_shapes();
    let cx = VisionCx::new();
    let concepts_only = VisionRuntime::builder().segmenter(sam3()).build().unwrap();
    let err = concepts_only.segment(&cx, &fixture.image, &fixture.prompts).await.unwrap_err();
    assert!(matches!(err, VisionError::Unsupported(_)), "{err}");
    assert!(err.to_string().contains("with_tracker"), "{err}");

    let both = VisionRuntime::builder().segmenter(sam3_with_tracker()).build().unwrap();
    let mut prompts = fixture.prompts.clone();
    prompts.insert(0, SegmentPrompt::Concept { text: "red".into() });
    let found = both.segment(&cx, &fixture.image, &prompts).await.unwrap().value;
    assert_eq!(found.len(), 3, "{found:?}");
    assert_eq!(found[0].label.as_deref(), Some("red"));
    for (region, object) in found[1..].iter().zip(&fixture.objects) {
        assert_eq!(region.label, None);
        let Shape::Mask(mask) = &region.shape else { panic!("{:?}", region.shape) };
        let bbox = mask.bbox().unwrap();
        assert!(within(bbox, object.bbox, 4.0), "{bbox:?} for {:?}", object.bbox);
    }

    let mask = adk_vision::Mask::empty(640, 480);
    let err = both.segment(&cx, &fixture.image, &[SegmentPrompt::Mask { mask }]).await.unwrap_err();
    assert!(matches!(err, VisionError::Unsupported(_)), "{err}");
}

#[test]
fn a_tracker_with_another_input_size_is_refused() {
    let mut tracking = manifest("sam3-tracker-stand-in", &TRACKER_FILES, "useRestricted");
    tracking.io.input = Some(adk_vision::InputSpec {
        width: 1024,
        height: 1024,
        letterbox: adk_vision::Letterbox::Stretch,
    });
    let tracker = Sam3Tracker::new(&accepted(&tracking), &OnnxOptions::default()).unwrap();
    let model = accepted(&manifest("sam3-stand-in", &CONCEPT_FILES, "useRestricted"));
    let err =
        Sam3::new(&model, &OnnxOptions::default()).unwrap().with_tracker(tracker).unwrap_err();
    assert!(matches!(err, VisionError::InvalidInput(_)), "{err}");
}

mod sam3_segmenter_conformance {
    use super::*;

    adk_vision::segmenter_conformance_tests! {
        backend = SegmenterTarget::Local(sam3()),
        fixture = shapes(),
    }
}

mod sam3_detector_conformance {
    use super::*;

    adk_vision::detector_conformance_tests! {
        backend = DetectorTarget::Local(sam3()),
        fixture = shapes(),
    }
}

mod sam3_tracker_conformance {
    use super::*;

    adk_vision::segmenter_conformance_tests! {
        backend = SegmenterTarget::Local(sam3_with_tracker()),
        fixture = boxed_shapes(),
    }
}
