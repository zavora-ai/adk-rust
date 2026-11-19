//! The model fetcher against a local fixture hub: downloads by revision, SHA-256 checks,
//! offline mode without a socket, gated repositories and the licence gate (spec 3.7 S6).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_vision::{
    AcceptanceStore, Digest, FetchConfig, InMemoryAcceptanceStore, LicenceAcceptance, LicenceClass,
    LicencePolicy, ModelFetcher, ModelManifest, VisionError,
};

const REPO: &str = "fixture/tiny-detector";
const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const WEIGHTS: &[u8] = b"not really an onnx graph, but bytes with a known digest";

/// A minimal Hugging Face resolve endpoint: ranged GETs with the headers `hf-hub` reads.
struct Hub {
    url: String,
    connections: Arc<AtomicUsize>,
    status: Arc<Mutex<Option<u16>>>,
}

impl Hub {
    fn start(files: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let connections = Arc::new(AtomicUsize::new(0));
        let status = Arc::new(Mutex::new(None));
        let (count, forced, files) =
            (Arc::clone(&connections), Arc::clone(&status), Arc::new(files));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                count.fetch_add(1, Ordering::SeqCst);
                let forced = *forced.lock().unwrap();
                let files = Arc::clone(&files);
                std::thread::spawn(move || serve(stream, &files, forced));
            }
        });
        Self { url, connections, status }
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    fn force(&self, status: u16) {
        *self.status.lock().unwrap() = Some(status);
    }
}

fn serve(stream: TcpStream, files: &HashMap<String, Vec<u8>>, forced: Option<u16>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    reader.read_line(&mut request).unwrap();
    let mut range = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
            range = Some(value.trim().to_string());
        }
    }
    let path = request.split_whitespace().nth(1).unwrap_or("/").trim_start_matches('/').to_string();
    let mut out = stream;
    if let Some(status) = forced {
        let _ = write!(
            out,
            "HTTP/1.1 {status} Refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        return;
    }
    let Some(body) = files.get(&path) else {
        let _ =
            write!(out, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    };
    let commit = path.split('/').nth(3).unwrap_or_default();
    let (start, end) = match range.as_deref().and_then(|r| r.split_once('-')) {
        Some((a, b)) => {
            let start: usize = a.parse().unwrap_or(0);
            let end =
                if b.is_empty() { body.len() - 1 } else { b.parse().unwrap_or(body.len() - 1) };
            (start, end.min(body.len() - 1))
        }
        None => (0, body.len() - 1),
    };
    let part = &body[start..=end];
    let _ = write!(
        out,
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\n\
         ETag: \"{}\"\r\nX-Repo-Commit: {commit}\r\nConnection: close\r\n\r\n",
        part.len(),
        body.len(),
        Digest::of(body).hex(),
    );
    let _ = out.write_all(part);
}

fn hub_with_weights(bytes: &[u8]) -> Hub {
    let mut files = HashMap::new();
    files.insert(format!("{REPO}/resolve/{REVISION}/onnx/model.onnx"), bytes.to_vec());
    Hub::start(files)
}

fn manifest(class: LicenceClass, sha256: Digest) -> ModelManifest {
    let class = serde_json::to_value(class).unwrap();
    ModelManifest::from_json(
        &serde_json::json!({
            "id": "tiny-detector",
            "task": "detect",
            "source": {"kind": "huggingFace", "repo": REPO, "revision": REVISION},
            "files": [{"path": "onnx/model.onnx", "sha256": sha256, "size": WEIGHTS.len()}],
            "licence": {"id": "Apache-2.0", "url": "https://www.apache.org/licenses/LICENSE-2.0", "class": class},
            "runtime": {"kind": "ort", "opset": 17}
        })
        .to_string(),
    )
    .unwrap()
}

fn fetcher(hub: &Hub, cache: &std::path::Path, offline: bool) -> ModelFetcher {
    let config = FetchConfig::from_lookup(|_| None)
        .with_cache_dir(cache)
        .with_endpoint(hub.url.clone())
        .with_offline(offline);
    ModelFetcher::new(config, Arc::new(InMemoryAcceptanceStore::new()))
}

fn snapshot_path(cache: &std::path::Path) -> PathBuf {
    cache
        .join(format!("models--{}", REPO.replace('/', "--")))
        .join("snapshots")
        .join(REVISION)
        .join("onnx/model.onnx")
}

#[test]
fn online_fetch_downloads_by_revision_and_verifies() {
    let hub = hub_with_weights(WEIGHTS);
    let cache = tempfile::tempdir().unwrap();
    let model = fetcher(&hub, cache.path(), false)
        .fetch_blocking(&manifest(LicenceClass::Permissive, Digest::of(WEIGHTS)))
        .unwrap();
    let file = model.file("onnx/model.onnx").unwrap();
    assert_eq!(file.digest(), Digest::of(WEIGHTS));
    assert_eq!(file.path(), snapshot_path(cache.path()));
    assert!(hub.connections() >= 1, "the first fetch downloads");
}

#[test]
fn offline_fetch_of_a_warm_cache_opens_no_socket() {
    let hub = hub_with_weights(WEIGHTS);
    let cache = tempfile::tempdir().unwrap();
    let manifest = manifest(LicenceClass::Permissive, Digest::of(WEIGHTS));
    fetcher(&hub, cache.path(), false).fetch_blocking(&manifest).unwrap();
    let before = hub.connections();
    let model = fetcher(&hub, cache.path(), true).fetch_blocking(&manifest).unwrap();
    assert_eq!(model.files().count(), 1);
    assert_eq!(hub.connections(), before, "an offline fetch opened a connection");
}

#[test]
fn offline_fetch_of_a_cold_cache_fails_without_a_socket() {
    let hub = hub_with_weights(WEIGHTS);
    let cache = tempfile::tempdir().unwrap();
    let err = fetcher(&hub, cache.path(), true)
        .fetch_blocking(&manifest(LicenceClass::Permissive, Digest::of(WEIGHTS)))
        .unwrap_err();
    assert!(matches!(err, VisionError::Offline { .. }), "{err}");
    assert!(err.to_string().contains("HF_HUB_OFFLINE"));
    assert_eq!(hub.connections(), 0);
}

#[test]
fn a_tampered_cached_file_is_refused_before_load() {
    let hub = hub_with_weights(WEIGHTS);
    let cache = tempfile::tempdir().unwrap();
    let manifest = manifest(LicenceClass::Permissive, Digest::of(WEIGHTS));
    fetcher(&hub, cache.path(), false).fetch_blocking(&manifest).unwrap();
    let mut tampered = WEIGHTS.to_vec();
    tampered[0] ^= 0xff;
    std::fs::write(snapshot_path(cache.path()), &tampered).unwrap();
    let err = fetcher(&hub, cache.path(), true).fetch_blocking(&manifest).unwrap_err();
    let VisionError::DigestMismatch { expected, actual, .. } = &err else {
        panic!("expected a digest mismatch, got {err}");
    };
    assert_eq!((*expected, *actual), (Digest::of(WEIGHTS), Digest::of(&tampered)));
    let message = err.to_string();
    assert!(message.contains(&expected.to_string()) && message.contains(&actual.to_string()));
}

#[test]
fn a_download_that_does_not_match_its_manifest_is_refused() {
    let mut other = WEIGHTS.to_vec();
    other.reverse();
    let hub = hub_with_weights(&other);
    let cache = tempfile::tempdir().unwrap();
    let err = fetcher(&hub, cache.path(), false)
        .fetch_blocking(&manifest(LicenceClass::Permissive, Digest::of(WEIGHTS)))
        .unwrap_err();
    assert!(matches!(err, VisionError::DigestMismatch { .. }), "{err}");
}

#[test]
fn a_gated_repository_needs_licence_acceptance() {
    for status in [401, 403] {
        let hub = hub_with_weights(WEIGHTS);
        hub.force(status);
        let cache = tempfile::tempdir().unwrap();
        let err = fetcher(&hub, cache.path(), false)
            .fetch_blocking(&manifest(LicenceClass::Permissive, Digest::of(WEIGHTS)))
            .unwrap_err();
        assert!(matches!(err, VisionError::LicenceAcceptanceRequired { .. }), "{status}: {err}");
        let message = err.to_string();
        assert!(message.contains("licence acceptance required"), "{message}");
        assert!(message.contains(&format!("{}/{REPO}", hub.url)), "{message}");
    }
}

#[test]
fn use_restricted_models_need_a_recorded_acceptance_before_any_request() {
    let hub = hub_with_weights(WEIGHTS);
    let cache = tempfile::tempdir().unwrap();
    let manifest = manifest(LicenceClass::UseRestricted, Digest::of(WEIGHTS));
    let store = Arc::new(InMemoryAcceptanceStore::new());
    let config = FetchConfig::from_lookup(|_| None)
        .with_cache_dir(cache.path())
        .with_endpoint(hub.url.clone());
    let fetcher = ModelFetcher::new(config, store.clone());

    let err = fetcher.fetch_blocking(&manifest).unwrap_err();
    assert!(matches!(err, VisionError::LicenceAcceptanceRequired { .. }), "{err}");
    assert!(err.to_string().contains("https://www.apache.org/licenses/LICENSE-2.0"));
    assert_eq!(hub.connections(), 0, "the licence gate runs before any request");

    store.record(LicenceAcceptance::of(&manifest, "alice@example.com")).unwrap();
    fetcher.fetch_blocking(&manifest).unwrap();
}

#[test]
fn non_commercial_models_need_the_policy_to_name_them() {
    let hub = hub_with_weights(WEIGHTS);
    let cache = tempfile::tempdir().unwrap();
    let manifest = manifest(LicenceClass::NonCommercial, Digest::of(WEIGHTS));
    let err = fetcher(&hub, cache.path(), false).fetch_blocking(&manifest).unwrap_err();
    assert!(matches!(err, VisionError::LicenceNotAllowed { .. }), "{err}");
    assert_eq!(hub.connections(), 0);
    fetcher(&hub, cache.path(), false)
        .with_policy(LicencePolicy::new().allowing("tiny-detector"))
        .fetch_blocking(&manifest)
        .unwrap();
}

#[test]
fn a_missing_file_is_a_fetch_error_naming_the_revision() {
    let hub = Hub::start(HashMap::new());
    let cache = tempfile::tempdir().unwrap();
    let err = fetcher(&hub, cache.path(), false)
        .fetch_blocking(&manifest(LicenceClass::Permissive, Digest::of(WEIGHTS)))
        .unwrap_err();
    assert!(matches!(err, VisionError::Fetch { .. }), "{err}");
    assert!(err.to_string().contains(REVISION), "{err}");
}

#[tokio::test]
async fn async_fetch_runs_on_a_blocking_thread() {
    let hub = hub_with_weights(WEIGHTS);
    let cache = tempfile::tempdir().unwrap();
    let model = fetcher(&hub, cache.path(), false)
        .fetch(&manifest(LicenceClass::Permissive, Digest::of(WEIGHTS)))
        .await
        .unwrap();
    assert_eq!(model.manifest().id, "tiny-detector");
}

#[test]
fn configuration_follows_the_python_hub_conventions() {
    let env: HashMap<&str, &str> = [
        ("HF_HUB_CACHE", "/cache/hub"),
        ("HF_HOME", "/home/hf"),
        ("HF_HUB_OFFLINE", "TRUE"),
        ("HF_ENDPOINT", "https://mirror.example"),
    ]
    .into();
    let config = FetchConfig::from_lookup(|k| env.get(k).map(|v| v.to_string()));
    assert_eq!(config.cache_dir, PathBuf::from("/cache/hub"));
    assert!(config.offline);
    assert_eq!(config.endpoint, "https://mirror.example");
    assert_eq!(config.token, None);

    let home: HashMap<&str, &str> = [("HF_HOME", "/home/hf"), ("HF_HUB_OFFLINE", "0")].into();
    let config = FetchConfig::from_lookup(|k| home.get(k).map(|v| v.to_string()));
    assert_eq!(config.cache_dir, PathBuf::from("/home/hf/hub"));
    assert!(!config.offline);
    assert_eq!(config.endpoint, "https://huggingface.co");
    assert!(!format!("{:?}", config.with_token(Some("hf_secret".into()))).contains("hf_secret"));
}

#[test]
fn local_sources_are_verified_in_place() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("onnx")).unwrap();
    std::fs::write(dir.path().join("onnx/model.onnx"), WEIGHTS).unwrap();
    let mut manifest = manifest(LicenceClass::Permissive, Digest::of(WEIGHTS));
    manifest.source = adk_vision::ModelSource::Local { dir: dir.path().to_path_buf() };
    let hub = hub_with_weights(WEIGHTS);
    let model = fetcher(&hub, dir.path(), true).fetch_blocking(&manifest).unwrap();
    assert_eq!(model.files().next().unwrap().digest(), Digest::of(WEIGHTS));
    assert_eq!(hub.connections(), 0);
}
