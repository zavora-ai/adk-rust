//! S6, weights you can trust: the desk's models fetched by revision and verified by SHA-256,
//! an offline fetch from a warm cache that opens no socket, a tampered file refused before
//! ONNX Runtime loads it, a use-restricted model held until a person's acceptance is
//! recorded, and a built-in manifest with no non-commercial or copyleft model.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_vision::{
    AcceptanceStore, Digest, FetchConfig, InMemoryAcceptanceStore, LicenceAcceptance,
    LicenceClass, ManifestSet, ModelFetcher, ModelManifest, ModelSource, PpOcrReader, PpOcrV6Size,
    VisionError,
};
use anyhow::{Context, Result};
use serde_json::json;

use crate::Checks;
use crate::fixtures::{PERSON_DETECTOR, fetcher};

const DESK_MODELS: [&str; 3] = ["pp-ocrv6-tiny-det", "pp-ocrv6-tiny-rec", PERSON_DETECTOR];
const RESTRICTED_REPO: &str = "fixture/restricted-detector";
const RESTRICTED_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const RESTRICTED_LICENCE: &str = "https://licences.example/restricted-model-licence-1.0";
const WEIGHTS: &[u8] = b"not an onnx graph, but bytes with a known digest";

/// A minimal Hugging Face resolve endpoint that counts the connections it accepts.
struct Hub {
    url: String,
    connections: Arc<AtomicUsize>,
}

impl Hub {
    fn start(files: HashMap<String, Vec<u8>>) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}", listener.local_addr()?);
        let connections = Arc::new(AtomicUsize::new(0));
        let (count, files) = (Arc::clone(&connections), Arc::new(files));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                count.fetch_add(1, Ordering::SeqCst);
                let files = Arc::clone(&files);
                std::thread::spawn(move || serve(stream, &files));
            }
        });
        Ok(Self { url, connections })
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

/// Ranged GETs with the headers `hf-hub` reads.
fn serve(stream: TcpStream, files: &HashMap<String, Vec<u8>>) {
    let Ok(clone) = stream.try_clone() else { return };
    let mut reader = BufReader::new(clone);
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    let mut range = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
            range = Some(value.trim().to_string());
        }
    }
    let path = request.split_whitespace().nth(1).unwrap_or("/").trim_start_matches('/');
    let mut out = stream;
    let Some(body) = files.get(path) else {
        let _ = write!(out, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    };
    let commit = path.split('/').nth(3).unwrap_or_default();
    let (start, end) = match range.as_deref().and_then(|r| r.split_once('-')) {
        Some((a, b)) => {
            let start: usize = a.parse().unwrap_or(0);
            let end = if b.is_empty() { body.len() - 1 } else { b.parse().unwrap_or(body.len() - 1) };
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

/// Copies `from` to `to`, following symbolic links, as the Hugging Face cache stores files
/// as links into its blobs.
fn copy_resolved(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let path = entry?.path();
        let target = to.join(path.file_name().context("a named entry")?);
        if std::fs::metadata(&path)?.is_dir() {
            copy_resolved(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

fn restricted_manifest() -> Result<ModelManifest> {
    Ok(ModelManifest::from_json(
        &json!({
            "id": "restricted-detector",
            "task": "detect",
            "source": { "kind": "huggingFace", "repo": RESTRICTED_REPO, "revision": RESTRICTED_REVISION },
            "files": [{ "path": "onnx/model.onnx", "sha256": Digest::of(WEIGHTS), "size": WEIGHTS.len() }],
            "licence": { "id": "LicenseRef-Restricted-Test", "url": RESTRICTED_LICENCE, "class": "useRestricted" },
            "runtime": { "kind": "ort", "opset": 17 },
        })
        .to_string(),
    )?)
}

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    checks.note("the built-in manifest against huggingface.co (cached in CI), a local fixture hub for the restricted model");
    let builtin = ManifestSet::builtin()?;
    let manifests: Vec<ModelManifest> = DESK_MODELS
        .iter()
        .map(|id| builtin.get(id).cloned().with_context(|| format!("{id} is built in")))
        .collect::<Result<_>>()?;

    // Fetched by revision and verified file by file.
    let online = fetcher();
    let mut verified = Vec::new();
    for manifest in &manifests {
        verified.push(online.fetch(manifest).await?);
    }
    let pinned = verified.iter().zip(&manifests).all(|(v, m)| {
        matches!(&m.source, ModelSource::HuggingFace { revision, .. } if revision.len() == 40)
            && m.files.iter().all(|f| v.file(&f.path).is_some_and(|got| got.digest() == f.sha256))
    });
    checks.check(
        "the desk's models are fetched by commit revision, each file's SHA-256 matching the manifest",
        pinned,
    );

    // Offline, with a warm cache: no socket.
    let silent = Hub::start(HashMap::new())?;
    let offline = ModelFetcher::new(
        FetchConfig::from_env().with_offline(true).with_endpoint(silent.url.clone()),
        Arc::new(InMemoryAcceptanceStore::new()),
    );
    let mut fetched = 0;
    for manifest in &manifests {
        if offline.fetch(manifest).await.is_ok() {
            fetched += 1;
        }
    }
    checks.check(
        "with HF_HUB_OFFLINE=1 and a warm cache the fetch succeeds and opens no socket",
        fetched == manifests.len() && silent.connections() == 0,
    );

    // A tampered file is refused before ONNX Runtime loads it.
    let det = &manifests[0];
    let ModelSource::HuggingFace { repo, revision } = &det.source else {
        anyhow::bail!("pp-ocrv6-tiny-det comes from Hugging Face");
    };
    let file = &det.files[0].path;
    let cached = online.cached(repo, revision, file).context("the detection model is cached")?;
    // A copy of the model's cache entry, `refs` and snapshot, with one byte of the file flipped.
    let cache = tempfile::tempdir()?;
    let entry = format!("models--{}", repo.replace('/', "--"));
    copy_resolved(&FetchConfig::from_env().cache_dir.join(&entry), &cache.path().join(&entry))?;
    let snapshot = cache.path().join(&entry).join("snapshots").join(revision);
    let mut bytes = std::fs::read(&cached)?;
    bytes[1_000] ^= 0x01;
    std::fs::write(snapshot.join(file), &bytes)?;
    let tampered = ModelFetcher::new(
        FetchConfig::from_env().with_offline(true).with_cache_dir(cache.path()),
        Arc::new(InMemoryAcceptanceStore::new()),
    );
    let refused = tampered.fetch(det).await;
    let names_both = matches!(&refused, Err(VisionError::DigestMismatch { .. }))
        && refused.as_ref().err().is_some_and(|e| {
            let text = e.to_string();
            text.contains(&det.files[0].sha256.hex()) && text.contains(&Digest::of(&bytes).hex())
        });
    checks.check(
        "a file whose SHA-256 differs is refused at fetch, naming both digests",
        names_both,
    );
    // Verified, then changed on disk before the load: the load checks it again.
    std::fs::write(snapshot.join(file), std::fs::read(&cached)?)?;
    let det_ok = tampered.fetch(det).await?;
    let rec_ok = online.fetch(&manifests[1]).await?;
    std::fs::write(snapshot.join(file), &bytes)?;
    let load = PpOcrReader::from_models(PpOcrV6Size::Tiny, &det_ok, &rec_ok);
    checks.check(
        "a verified file changed before the load is refused before ONNX Runtime opens it",
        matches!(load, Err(VisionError::DigestMismatch { .. })),
    );

    // A use-restricted model waits for a person's recorded acceptance.
    let manifest = restricted_manifest()?;
    let hub = Hub::start(HashMap::from([(
        format!("{RESTRICTED_REPO}/resolve/{RESTRICTED_REVISION}/onnx/model.onnx"),
        WEIGHTS.to_vec(),
    )]))?;
    let hub_cache = tempfile::tempdir()?;
    let acceptances = Arc::new(InMemoryAcceptanceStore::new());
    let gated = ModelFetcher::new(
        FetchConfig::from_lookup(|_| None)
            .with_endpoint(hub.url.clone())
            .with_cache_dir(hub_cache.path()),
        acceptances.clone(),
    );
    let held = gated.fetch(&manifest).await;
    let asks = matches!(&held, Err(VisionError::LicenceAcceptanceRequired { .. }))
        && held.as_ref().err().is_some_and(|e| e.to_string().contains(RESTRICTED_LICENCE));
    checks.check(
        "the restricted model fails with \"licence acceptance required\" naming its licence URL, before any request",
        asks && hub.connections() == 0,
    );
    acceptances.record(LicenceAcceptance::of(&manifest, "ana"))?;
    let accepted = gated.fetch(&manifest).await;
    checks.check(
        "once a person's acceptance is recorded it is fetched and verified",
        accepted.is_ok() && hub.connections() > 0,
    );

    let restricted: Vec<&str> = builtin
        .iter()
        .filter(|m| matches!(m.licence.class, LicenceClass::NonCommercial | LicenceClass::Copyleft))
        .map(|m| m.id.as_str())
        .collect();
    checks.check(
        &format!("the built-in manifest ({} models) lists no non-commercial or copyleft model", builtin.len()),
        restricted.is_empty(),
    );
    Ok(checks)
}
