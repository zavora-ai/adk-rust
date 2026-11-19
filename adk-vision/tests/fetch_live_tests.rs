//! Live fetches from huggingface.co (spec 3.7 S6). Ignored by default; run with
//!
//! ```text
//! HF_HUB_CACHE=/path/to/models cargo nextest run -p adk-vision --features fetch \
//!     --test fetch_live_tests --run-ignored only
//! ```
//!
//! The model is RF-DETR Nano's q4f16 ONNX export from `onnx-community/rfdetr_nano-ONNX`
//! (Apache-2.0), 18,996,625 bytes, pinned by commit and SHA-256.

use std::io::Read;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_vision::{
    Digest, FetchConfig, InMemoryAcceptanceStore, LicenceGate, LicencePolicy, ModelFetcher,
    ModelManifest, ModelSource, VerifiedModel, VisionError,
};

const FILE: &str = "onnx/model_q4f16.onnx";
const SHA256: &str = "sha256:42d8072bc00975405531e4e7f20dda40a6cb5ed67e345ecd764f92748463a996";

fn manifest() -> ModelManifest {
    ModelManifest::from_json(&format!(
        r#"{{
            "id": "rf-detr-n-q4f16",
            "task": "detect",
            "source": {{
                "kind": "huggingFace",
                "repo": "onnx-community/rfdetr_nano-ONNX",
                "revision": "eae21cee0687a91bcf9fa071605c48d7705d2d91"
            }},
            "files": [{{ "path": "{FILE}", "sha256": "{SHA256}", "size": 18996625 }}],
            "licence": {{
                "id": "Apache-2.0",
                "url": "https://huggingface.co/onnx-community/rfdetr_nano-ONNX",
                "class": "permissive"
            }},
            "runtime": {{ "kind": "ort", "opset": 17 }}
        }}"#
    ))
    .unwrap()
}

fn fetcher(config: FetchConfig) -> ModelFetcher {
    ModelFetcher::new(config, Arc::new(InMemoryAcceptanceStore::new()))
}

#[test]
#[ignore = "downloads 19 MB from huggingface.co into HF_HUB_CACHE"]
fn live_fetch_verifies_rf_detr_nano() {
    let config = FetchConfig::from_env().with_offline(false);
    let model = fetcher(config.clone()).fetch_blocking(&manifest()).unwrap();
    let file = model.file(FILE).unwrap();
    assert_eq!(file.digest().to_string(), SHA256);
    assert_eq!(file.size(), 18_996_625);
    assert!(file.path().starts_with(&config.cache_dir));
    eprintln!("verified {} {} ({} bytes)", file.path().display(), file.digest(), file.size());
}

#[test]
#[ignore = "needs the live fetch's warm cache in HF_HUB_CACHE"]
fn live_tampered_copy_is_refused() {
    let cached = fetcher(FetchConfig::from_env().with_offline(true))
        .fetch_blocking(&manifest())
        .unwrap()
        .file(FILE)
        .unwrap()
        .path()
        .to_path_buf();
    let copy = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(copy.path().join("onnx")).unwrap();
    let mut bytes = Vec::new();
    std::fs::File::open(&cached).unwrap().read_to_end(&mut bytes).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    std::fs::write(copy.path().join(FILE), &bytes).unwrap();

    let mut tampered = manifest();
    tampered.source = ModelSource::Local { dir: copy.path().to_path_buf() };
    let (policy, store) = (LicencePolicy::new(), InMemoryAcceptanceStore::new());
    let err = VerifiedModel::check_dir(&tampered, copy.path(), &LicenceGate::new(&policy, &store))
        .unwrap_err();
    let VisionError::DigestMismatch { expected, actual, .. } = &err else {
        panic!("expected a digest mismatch, got {err}");
    };
    assert_eq!(expected.to_string(), SHA256);
    assert_eq!(*actual, Digest::of(&bytes));
    eprintln!("refused: {err}");
}

#[test]
#[ignore = "needs the live fetch's warm cache in HF_HUB_CACHE"]
fn live_offline_fetch_opens_no_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    let watcher = std::thread::spawn(move || {
        for _ in 0..50 {
            if listener.accept().is_ok() {
                count.fetch_add(1, Ordering::SeqCst);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
    let config = FetchConfig::from_env().with_offline(true).with_endpoint(endpoint);
    let model = fetcher(config).fetch_blocking(&manifest()).unwrap();
    watcher.join().unwrap();
    assert_eq!(model.file(FILE).unwrap().digest().to_string(), SHA256);
    assert_eq!(accepted.load(Ordering::SeqCst), 0, "an offline fetch opened a connection");
}

#[test]
#[ignore = "needs the live fetch's warm cache in HF_HUB_CACHE and HF_HUB_OFFLINE=1"]
fn live_offline_fetch_from_the_environment() {
    let config = FetchConfig::from_env();
    assert!(config.offline, "set HF_HUB_OFFLINE=1");
    let model = fetcher(config).fetch_blocking(&manifest()).unwrap();
    assert_eq!(model.file(FILE).unwrap().digest().to_string(), SHA256);
    eprintln!("offline fetch verified {}", model.file(FILE).unwrap().path().display());
}
