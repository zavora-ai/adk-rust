//! Artifact scenarios: concurrent saves, name encoding, crash safety, and the user scope.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_artifact::{
    ArtifactService, FileArtifactService, InMemoryArtifactService, ListRequest, LoadRequest,
    SaveRequest, VersionsRequest,
};
use adk_core::{Llm, Part, ToolContext};
use futures::FutureExt;
use serde_json::{Value, json};

use crate::common::{APP, Services, USER, Verdict, agent, brief, json_tool, run_turn};
use crate::gcs_server::ScriptedGcs;

fn text(part: &Part) -> String {
    match part {
        Part::Text { text } => text.clone(),
        other => format!("{other:?}"),
    }
}

fn save_request(session: &str, name: &str, content: &str) -> SaveRequest {
    SaveRequest {
        app_name: APP.to_string(),
        user_id: USER.to_string(),
        session_id: session.to_string(),
        file_name: name.to_string(),
        part: Part::Text { text: content.to_string() },
        version: None,
    }
}

fn load_request(session: &str, name: &str, version: Option<i64>) -> LoadRequest {
    LoadRequest {
        app_name: APP.to_string(),
        user_id: USER.to_string(),
        session_id: session.to_string(),
        file_name: name.to_string(),
        version,
    }
}

async fn load_text(
    service: &dyn ArtifactService,
    session: &str,
    name: &str,
    version: Option<i64>,
) -> Result<String, String> {
    service
        .load(load_request(session, name, version))
        .await
        .map(|response| text(&response.part))
        .map_err(|error| error.to_string())
}

async fn list(service: &dyn ArtifactService, session: &str) -> Result<Vec<String>, String> {
    service
        .list(ListRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: session.to_string(),
        })
        .await
        .map(|response| response.file_names)
        .map_err(|error| error.to_string())
}

async fn versions(service: &dyn ArtifactService, session: &str, name: &str) -> Vec<i64> {
    service
        .versions(VersionsRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: session.to_string(),
            file_name: name.to_string(),
        })
        .await
        .map(|response| response.versions)
        .unwrap_or_default()
}

/// The artifact backends a scenario runs against.
pub enum Backend {
    File(tempfile::TempDir),
    Gcs(ScriptedGcs),
}

impl Backend {
    pub async fn file() -> anyhow::Result<Self> {
        Ok(Self::File(tempfile::tempdir()?))
    }

    pub async fn gcs() -> anyhow::Result<Self> {
        Ok(Self::Gcs(ScriptedGcs::start().await?))
    }

    fn service(&self) -> anyhow::Result<Arc<dyn ArtifactService>> {
        Ok(match self {
            Self::File(dir) => Arc::new(FileArtifactService::new(dir.path())?),
            Self::Gcs(gcs) => Arc::new(gcs.service()?),
        })
    }
}

// ---------------------------------------------------------------------------
// SS8: four parallel tool calls each save a version of the same artifact
// ---------------------------------------------------------------------------

pub async fn parallel_saves(model: Arc<dyn Llm>, backend: Backend) -> anyhow::Result<Verdict> {
    let service = backend.service()?;
    let saved: Arc<Mutex<Vec<(String, i64)>>> = Arc::default();
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let tool = {
        let (saved, in_flight, peak) = (saved.clone(), in_flight.clone(), peak.clone());
        json_tool(
            "save_note",
            "Saves one note as a new version of the artifact notes.txt and returns the version.",
            json!({
                "type": "object",
                "properties": {"content": {"type": "string", "description": "The note text"}},
                "required": ["content"]
            }),
            move |ctx: Arc<dyn ToolContext>, args: Value| {
                let (saved, in_flight, peak) = (saved.clone(), in_flight.clone(), peak.clone());
                async move {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    let content = args["content"].as_str().unwrap_or_default().to_string();
                    let artifacts = ctx
                        .artifacts()
                        .ok_or_else(|| adk_core::AdkError::tool("no artifact service"))?;
                    let result =
                        artifacts.save("notes.txt", &Part::Text { text: content.clone() }).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    let version = result?;
                    saved.lock().unwrap().push((content, version));
                    Ok(json!({ "version": version }))
                }
                .boxed()
            },
        )
    };
    let agent = agent(
        model,
        "You save notes with the save_note tool. When asked for several saves, issue all of the \
         tool calls together in one response, never one at a time.",
        vec![tool],
    )?;
    let answer = run_turn(
        agent,
        Services { artifacts: Some(service.clone()), memory: None },
        "s-parallel",
        "Call save_note four times in parallel, in a single response, with the contents \"one\", \
         \"two\", \"three\" and \"four\". Then tell me the versions the tool returned.",
    )
    .await?;

    let saved = saved.lock().unwrap().clone();
    if saved.len() != 4 || peak.load(Ordering::SeqCst) < 2 {
        return Ok(Verdict::Skip(format!(
            "model made {} save calls with at most {} in flight; the race needs 4 overlapping \
             calls: {}",
            saved.len(),
            peak.load(Ordering::SeqCst),
            brief(&answer)
        )));
    }
    let returned: BTreeSet<i64> = saved.iter().map(|(_, version)| *version).collect();
    let stored = versions(service.as_ref(), "s-parallel", "notes.txt").await;
    let mut contents = BTreeSet::new();
    for (_, version) in &saved {
        if let Ok(content) =
            load_text(service.as_ref(), "s-parallel", "notes.txt", Some(*version)).await
        {
            contents.insert(content);
        }
    }
    let evidence = format!(
        "4 overlapping saves returned versions {returned:?}; stored versions {stored:?}; \
         distinct contents loadable by returned version: {}",
        contents.len()
    );
    if returned.len() == 4 && stored.len() == 4 && contents.len() == 4 {
        Ok(Verdict::Pass(evidence))
    } else {
        Ok(Verdict::Fail(evidence))
    }
}

// ---------------------------------------------------------------------------
// SS8: a direct concurrent harness on every backend
// ---------------------------------------------------------------------------

/// Counts, over `rounds` races of `writers` simultaneous saves: versions that a successful
/// save reported but that are not stored, version numbers handed to more than one save, and
/// failed saves.
async fn race(
    service: Arc<dyn ArtifactService>,
    rounds: usize,
    writers: usize,
) -> (usize, usize, usize) {
    let (mut lost, mut duplicated, mut errors) = (0, 0, 0);
    for round in 0..rounds {
        let name = format!("race-{round}.txt");
        let barrier = Arc::new(tokio::sync::Barrier::new(writers));
        let mut tasks = Vec::new();
        for writer in 0..writers {
            let (service, barrier, name) = (service.clone(), barrier.clone(), name.clone());
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                service.save(save_request("s-race", &name, &format!("writer {writer}"))).await
            }));
        }
        let mut returned = BTreeSet::new();
        let mut successes = 0;
        for task in tasks {
            if let Ok(Ok(response)) = task.await {
                successes += 1;
                returned.insert(response.version);
            }
        }
        let stored = versions(service.as_ref(), "s-race", &name).await.len();
        errors += writers - successes;
        lost += successes.saturating_sub(stored);
        duplicated += successes - returned.len();
    }
    (lost, duplicated, errors)
}

pub async fn concurrent_saves() -> anyhow::Result<Verdict> {
    let file = tempfile::tempdir()?;
    let gcs = ScriptedGcs::start().await?;
    let backends: Vec<(&str, Arc<dyn ArtifactService>, usize)> = vec![
        ("memory", Arc::new(InMemoryArtifactService::new()), 200),
        ("file", Arc::new(FileArtifactService::new(file.path())?), 20),
        ("gcs", Arc::new(gcs.service()?), 5),
    ];
    let writers = 16;
    let mut failed = false;
    let mut evidence = Vec::new();
    for (label, service, rounds) in backends {
        let (lost, duplicated, errors) = race(service, rounds, writers).await;
        failed |= lost > 0 || duplicated > 0 || errors > 0;
        evidence.push(format!(
            "{label}: {} saves, {lost} versions lost, {duplicated} duplicate version numbers, \
             {errors} errors",
            rounds * writers
        ));
    }
    let evidence = evidence.join("; ");
    Ok(if failed { Verdict::Fail(evidence) } else { Verdict::Pass(evidence) })
}

// ---------------------------------------------------------------------------
// SS10: `a:b` and `a__b` are different artifacts
// ---------------------------------------------------------------------------

fn save_artifact_tool() -> Arc<dyn adk_core::Tool> {
    json_tool(
        "save_artifact",
        "Saves text as a new version of the named artifact.",
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Artifact name, used verbatim"},
                "content": {"type": "string", "description": "Text to store"}
            },
            "required": ["name", "content"]
        }),
        |ctx: Arc<dyn ToolContext>, args: Value| {
            async move {
                let artifacts = ctx
                    .artifacts()
                    .ok_or_else(|| adk_core::AdkError::tool("no artifact service"))?;
                let name = args["name"].as_str().unwrap_or_default().to_string();
                let content = args["content"].as_str().unwrap_or_default().to_string();
                let version = artifacts.save(&name, &Part::Text { text: content }).await?;
                Ok(json!({ "saved": name, "version": version }))
            }
            .boxed()
        },
    )
}

fn load_artifact_tool() -> Arc<dyn adk_core::Tool> {
    json_tool(
        "load_artifact",
        "Loads the latest version of the named artifact.",
        json!({
            "type": "object",
            "properties": {"name": {"type": "string", "description": "Artifact name, used verbatim"}},
            "required": ["name"]
        }),
        |ctx: Arc<dyn ToolContext>, args: Value| {
            async move {
                let artifacts = ctx
                    .artifacts()
                    .ok_or_else(|| adk_core::AdkError::tool("no artifact service"))?;
                let name = args["name"].as_str().unwrap_or_default().to_string();
                let part = artifacts.load(&name).await?;
                Ok(json!({ "name": name, "content": text(&part) }))
            }
            .boxed()
        },
    )
}

pub async fn name_collision(model: Arc<dyn Llm>) -> anyhow::Result<Verdict> {
    let dir = tempfile::tempdir()?;
    let service: Arc<dyn ArtifactService> = Arc::new(FileArtifactService::new(dir.path())?);
    let agent = agent(
        model,
        "You store and read artifacts with the save_artifact and load_artifact tools. Use \
         artifact names exactly as the user writes them. Run tool calls one at a time.",
        vec![save_artifact_tool(), load_artifact_tool()],
    )?;
    let answer = run_turn(
        agent,
        Services { artifacts: Some(service.clone()), memory: None },
        "s-names",
        "First save the text \"alpha\" as the artifact named a:b. Then save the text \"beta\" as \
         the artifact named a__b. Finally load the artifact a:b and tell me exactly what it \
         contains.",
    )
    .await?;
    let colon = load_text(service.as_ref(), "s-names", "a:b", None).await;
    let underscores = load_text(service.as_ref(), "s-names", "a__b", None).await;
    let names = list(service.as_ref(), "s-names").await;
    let evidence = format!(
        "load(a:b) = {colon:?}; load(a__b) = {underscores:?}; list = {names:?}; model: {}",
        brief(&answer)
    );
    let listed_both = names.as_ref().is_ok_and(|names| {
        names.contains(&"a:b".to_string()) && names.contains(&"a__b".to_string())
    });
    if underscores.is_err() && colon.is_err() {
        return Ok(Verdict::Skip(format!("the model saved neither artifact: {evidence}")));
    }
    Ok(if colon.as_deref() == Ok("alpha") && underscores.as_deref() == Ok("beta") && listed_both {
        Verdict::Pass(evidence)
    } else {
        Verdict::Fail(evidence)
    })
}

// ---------------------------------------------------------------------------
// SS10: artifacts written in the old on-disk layout stay loadable
// ---------------------------------------------------------------------------

fn write_legacy(base: &Path, segments: &[&str], content: &str) -> anyhow::Result<()> {
    let mut path = base.to_path_buf();
    for segment in &segments[..segments.len() - 1] {
        path.push(segment);
    }
    std::fs::create_dir_all(&path)?;
    path.push(segments[segments.len() - 1]);
    std::fs::write(path, serde_json::to_vec(&Part::Text { text: content.to_string() })?)?;
    Ok(())
}

pub async fn legacy_layout() -> anyhow::Result<Verdict> {
    let dir = tempfile::tempdir()?;
    let base = dir.path();
    // The layout earlier releases wrote: `:` became `__`, user scope lived in `_user_scoped_`.
    write_legacy(base, &[APP, USER, "s-legacy", "report__q1", "v1.json"], "quarterly")?;
    write_legacy(base, &[APP, USER, "s-legacy", "plain.txt", "v1.json"], "plain")?;
    write_legacy(base, &[APP, USER, "_user_scoped_", "user__prefs", "v1.json"], "prefs v1")?;
    write_legacy(base, &[APP, USER, "_user_scoped_", "user__prefs", "v2.json"], "prefs v2")?;

    let service = FileArtifactService::new(base)?;
    let report = load_text(&service, "s-legacy", "report:q1", None).await;
    let plain = load_text(&service, "s-legacy", "plain.txt", None).await;
    let prefs = load_text(&service, "s-legacy", "user:prefs", None).await;
    let prefs_v1 = load_text(&service, "s-other", "user:prefs", Some(1)).await;
    let names = list(&service, "s-legacy").await;
    let next = service
        .save(save_request("s-legacy", "user:prefs", "prefs v3"))
        .await
        .map(|response| response.version)
        .map_err(|error| error.to_string());
    let latest = load_text(&service, "s-other", "user:prefs", None).await;
    let all_versions = versions(&service, "s-legacy", "user:prefs").await;

    let expected_names: Vec<String> =
        ["plain.txt", "report:q1", "user:prefs"].iter().map(ToString::to_string).collect();
    let evidence = format!(
        "report:q1 = {report:?}; plain.txt = {plain:?}; user:prefs = {prefs:?} (v1 {prefs_v1:?}); \
         list = {names:?}; next save -> {next:?}, latest {latest:?}, versions {all_versions:?}"
    );
    let ok = report.as_deref() == Ok("quarterly")
        && plain.as_deref() == Ok("plain")
        && prefs.as_deref() == Ok("prefs v2")
        && prefs_v1.as_deref() == Ok("prefs v1")
        && names.as_ref() == Ok(&expected_names)
        && next == Ok(3)
        && latest.as_deref() == Ok("prefs v3")
        && all_versions.len() == 3;
    Ok(if ok { Verdict::Pass(evidence) } else { Verdict::Fail(evidence) })
}

// ---------------------------------------------------------------------------
// SS18: a writer killed mid-save leaves the previous version readable
// ---------------------------------------------------------------------------

const BIG_ARTIFACT_BYTES: usize = 160 * 1024 * 1024;

/// Child-process body for `--crash-writer <dir>`: saves one large version, then exits.
pub async fn crash_writer(base: &str) -> anyhow::Result<()> {
    let service = FileArtifactService::new(base)?;
    service.save(save_request("s-crash", "big.txt", &"x".repeat(BIG_ARTIFACT_BYTES))).await?;
    Ok(())
}

pub async fn crash_mid_write() -> anyhow::Result<Verdict> {
    let dir = tempfile::tempdir()?;
    let service = FileArtifactService::new(dir.path())?;
    service.save(save_request("s-crash", "big.txt", "small and complete")).await?;

    let artifact_dir = std::fs::read_dir(dir.path().join(APP).join(USER).join("s-crash"))?
        .next()
        .ok_or_else(|| anyhow::anyhow!("artifact directory missing"))??
        .path();
    let before: BTreeSet<_> =
        std::fs::read_dir(&artifact_dir)?.filter_map(|e| Some(e.ok()?.file_name())).collect();

    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg("--crash-writer")
        .arg(dir.path())
        .spawn()?;
    let started = Instant::now();
    let mut killed_while_writing = false;
    loop {
        let now: BTreeSet<_> =
            std::fs::read_dir(&artifact_dir)?.filter_map(|e| Some(e.ok()?.file_name())).collect();
        if now.len() > before.len() {
            // The writer has started its file; kill it before it can finish.
            child.kill()?;
            killed_while_writing = true;
            break;
        }
        if child.try_wait()?.is_some() || started.elapsed() > Duration::from_secs(60) {
            break;
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    let _ = child.wait();
    if !killed_while_writing {
        return Ok(Verdict::Skip("the writer finished before it could be killed".to_string()));
    }

    let files: Vec<String> = std::fs::read_dir(&artifact_dir)?
        .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    let latest = service.load(load_request("s-crash", "big.txt", None)).await;
    let evidence = match &latest {
        Ok(response) => {
            let len = text(&response.part).len();
            format!("killed mid-save; files {files:?}; latest version loads ({len} bytes)")
        }
        Err(error) => format!("killed mid-save; files {files:?}; latest version fails: {error}"),
    };
    let complete = latest.as_ref().is_ok_and(|response| {
        let content = text(&response.part);
        content == "small and complete" || content.len() == BIG_ARTIFACT_BYTES
    });
    Ok(if complete { Verdict::Pass(evidence) } else { Verdict::Fail(evidence) })
}

// ---------------------------------------------------------------------------
// SS21: a session that shares the user scope's name does not leak into other sessions
// ---------------------------------------------------------------------------

pub async fn reserved_session(model: Arc<dyn Llm>) -> anyhow::Result<Verdict> {
    let memory: Arc<dyn ArtifactService> = Arc::new(InMemoryArtifactService::new());
    let agent = agent(
        model,
        "You store artifacts with the save_artifact tool. Use artifact names exactly as the user \
         writes them. If the tool reports an error, say so.",
        vec![save_artifact_tool()],
    )?;
    let answer = run_turn(
        agent,
        Services { artifacts: Some(memory.clone()), memory: None },
        "user",
        "Save the text \"private draft\" as the artifact named draft.txt.",
    )
    .await?;
    let live_leak = list(memory.as_ref(), "s-other").await;

    // The same request against the file backend, whose user scope directory was
    // `_user_scoped_`, and against a scripted GCS bucket, whose user scope segment is `user`.
    let dir = tempfile::tempdir()?;
    let file = FileArtifactService::new(dir.path())?;
    let gcs_server = ScriptedGcs::start().await?;
    let gcs = gcs_server.service()?;
    let mut scripted = Vec::new();
    for (label, service, session) in [
        ("file", &file as &dyn ArtifactService, "user"),
        ("file", &file as &dyn ArtifactService, "_user_scoped_"),
        ("gcs", &gcs as &dyn ArtifactService, "user"),
    ] {
        let saved = service.save(save_request(session, "draft.txt", "private draft")).await;
        let other = list(service, "s-other").await;
        scripted.push((label, session, saved.map(|r| r.version).map_err(|e| e.to_string()), other));
    }

    let leaked = |names: &Result<Vec<String>, String>| {
        names.as_ref().is_ok_and(|names| names.contains(&"draft.txt".to_string()))
    };
    let mut evidence = vec![format!(
        "memory (live, session \"user\"): other session lists {live_leak:?}; model: {}",
        brief(&answer)
    )];
    let mut failed = leaked(&live_leak);
    for (label, session, saved, other) in &scripted {
        failed |= leaked(other);
        evidence.push(format!(
            "{label} (scripted, session {session:?}): save -> {saved:?}, other session lists {other:?}"
        ));
    }
    let evidence = evidence.join("; ");
    Ok(if failed { Verdict::Fail(evidence) } else { Verdict::Pass(evidence) })
}
