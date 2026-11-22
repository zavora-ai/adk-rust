//! One real run (spec 3.7 §11): a camera on this machine read by `DeviceSource`, and
//! RF-DETR Nano detecting on the sampled frames through the stream pipeline. Ignored by
//! default: it opens a real camera (on macOS, after the terminal is allowed to use it), and
//! fetches RF-DETR Nano into the Hugging Face cache, verified against the built-in manifest.
//! It prints the labels found and stores no frame.
//!
//! ```text
//! ADK_VISION_TEST_DEVICE=0 HF_HUB_CACHE=/path/to/models cargo nextest run -p adk-vision \
//!     --features device,onnx --test stream_device_live_tests --run-ignored only --no-capture
//! ```
//!
//! `ADK_VISION_TEST_DEVICE` is an index, a name or a `/dev/videoN` node, as
//! `adk-rust vision sources list` shows them (default `0`).

#![cfg(all(feature = "device", feature = "onnx"))]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_vision::onnx::{OnnxOptions, RfDetr};
use adk_vision::stream::{
    DeviceSelector, DeviceSource, NoRedaction, Observation, ObservationSink, StreamPipeline,
};
use adk_vision::{
    DetectQuery, FetchConfig, FrameSourceId, InMemoryAcceptanceStore, ManifestSet, ModelFetcher,
    Sampling, VisionCx, VisionRuntime,
};
use tokio_util::sync::CancellationToken;

/// One processed frame: its index, the labels found with their counts, its size and the
/// detector's time.
type Seen = (u64, BTreeMap<String, usize>, (u32, u32), u64);

/// Keeps what each frame showed, and stops the run after `limit` frames.
struct Labels {
    limit: usize,
    seen: AtomicUsize,
    stop: CancellationToken,
    found: Mutex<Vec<Seen>>,
}

#[async_trait::async_trait]
impl ObservationSink for Labels {
    async fn observe(&self, _cx: &VisionCx, observation: Observation) -> adk_vision::Result<()> {
        let mut labels = BTreeMap::new();
        for region in &observation.regions {
            *labels.entry(region.label.clone().unwrap_or_default()).or_default() += 1;
        }
        let latency = observation.calls.iter().map(|c| c.duration_ms).sum();
        let size = observation.image.pixels().size();
        self.found.lock().unwrap().push((observation.frame.index, labels, size, latency));
        if self.seen.fetch_add(1, Ordering::SeqCst) + 1 >= self.limit {
            self.stop.cancel();
        }
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "opens a real camera and runs RF-DETR Nano; see the module docs"]
async fn rf_detr_detects_on_frames_from_a_local_camera() {
    let manifest = ManifestSet::builtin().unwrap().get("rf-detr-n").cloned().unwrap();
    let model = tokio::task::spawn_blocking(move || {
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()))
            .fetch_blocking(&manifest)
    })
    .await
    .unwrap()
    .unwrap();
    let detector =
        Arc::new(RfDetr::new(&model, &OnnxOptions::default()).unwrap().with_threshold(0.5));
    let runtime = Arc::new(VisionRuntime::builder().detector(detector).build().unwrap());

    let device = std::env::var("ADK_VISION_TEST_DEVICE").unwrap_or_else(|_| "0".into());
    let source = Arc::new(DeviceSource::new(
        FrameSourceId("local-camera".into()),
        DeviceSelector::parse(&device),
    ));
    let stop = CancellationToken::new();
    let sink = Arc::new(Labels {
        limit: 5,
        seen: AtomicUsize::new(0),
        stop: stop.clone(),
        found: Mutex::new(Vec::new()),
    });
    let summary = StreamPipeline::builder(source, Arc::new(NoRedaction))
        .sampling(Sampling { fps: 1.0, max_long_edge: Some(640) })
        .detector(runtime, DetectQuery::Classes(Vec::new()))
        .sink(sink.clone())
        .build()
        .unwrap()
        .run(&VisionCx::new().with_cancellation(stop))
        .await
        .unwrap();

    let found = sink.found.lock().unwrap().clone();
    for (index, labels, size, latency) in &found {
        eprintln!("frame {index} {size:?}: {labels:?} in {latency} ms");
    }
    eprintln!("device {device} + rf-detr-n: {summary:?}");
    assert_eq!(found.len(), 5, "{summary:?}");
    assert!(found.iter().all(|(_, _, (w, h), _)| (*w).max(*h) <= 640));
}
