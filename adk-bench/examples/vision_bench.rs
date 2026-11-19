//! The vision benchmark and eval agent (spec 3.7 §7.16, §7.17).
//!
//! ```text
//! # The benchmark: every backend this build has, the table as Markdown and JSON
//! HF_HUB_CACHE=/path/to/models cargo run -p adk-bench --profile ci \
//!     --features vision-onnx,vision-ocr,vision-cloud --example vision_bench -- \
//!     run --out vision-bench.md --json vision-bench.json
//!
//! # One backend as a `cargo adk eval --agent-cmd`
//! cargo adk eval adk-vision/tests/fixtures/eval/documents-read.test.json \
//!     --criteria adk-vision/tests/fixtures/eval/criteria/read.json \
//!     --agent-cmd "target/ci/examples/vision_bench serve pp-ocrv6-tiny"
//!
//! # The backends this build has
//! cargo run -p adk-bench --features vision-onnx --example vision_bench -- list
//! ```
//!
//! `run` options: `--backends a,b` (default: every local backend, plus the recorded cloud
//! rows with `vision-cloud`), `--runs N`, `--warmup N`, `--edges 512,1024,2048`,
//! `--no-recorded`, and `--live claude,nvidia,sam` for one live call per named provider
//! (`ANTHROPIC_API_KEY`, `NVIDIA_API_KEY`, `META_API_KEY`). Logs go to stderr.

use std::path::Path;
use std::process::ExitCode;

use adk_bench::vision::backends::{self, Loaded};
use adk_bench::vision::{
    EmbedWorkload, FixtureSet, MachineInfo, VisionBenchConfig, VisionBenchReport, VisionBenchRow,
    eval_dir, run_backend, run_embedder,
};
use adk_vision::eval::serve_eval_protocol;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]).await,
        Some("serve") => serve(args.get(1).map(String::as_str)).await,
        Some("list") => {
            for spec in backends::local_specs() {
                println!("{}\t{}", spec.name, spec.workload);
            }
            #[cfg(feature = "vision-cloud")]
            for name in cloud::RECORDED {
                println!("{name}\trecorded");
            }
            Ok(())
        }
        _ => Err("usage: vision_bench run [options] | serve <backend> | list".to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn list(value: Option<&str>) -> Vec<String> {
    value
        .map(|v| v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

/// The versions of the runtime crates this binary was built with, from the workspace lock.
fn runtime_versions(machine: MachineInfo) -> MachineInfo {
    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.lock");
    let text = std::fs::read_to_string(lock).unwrap_or_default();
    let mut machine = machine;
    for name in ["ort", "oar-ocr", "oar-ocr-vl", "candle-core", "image", "tokenizers"] {
        let needle = format!("name = \"{name}\"\nversion = \"");
        if let Some(at) = text.find(&needle) {
            let rest = &text[at + needle.len()..];
            if let Some(end) = rest.find('"') {
                machine = machine.with_runtime(name, &rest[..end]);
            }
        }
    }
    if let Some(version) = onnxruntime_version() {
        machine = machine.with_runtime("onnxruntime", version);
    }
    machine.with_runtime("profile", if cfg!(debug_assertions) { "debug" } else { "optimised" })
}

/// The onnxruntime library version, from the versioned library `ort` copies beside the binary.
fn onnxruntime_version() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let mut dirs = vec![exe.parent()?.to_path_buf()];
    dirs.extend(exe.parent()?.parent().map(Path::to_path_buf));
    for dir in dirs {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let version = name
                .strip_prefix("libonnxruntime.")
                .and_then(|rest| rest.strip_suffix(".dylib"))
                .or_else(|| name.strip_prefix("libonnxruntime.so."));
            if let Some(v) =
                version.filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
            {
                return Some(v.to_string());
            }
        }
    }
    None
}

async fn run(args: &[String]) -> Result<(), String> {
    let mut config = VisionBenchConfig::default();
    if let Some(runs) = option(args, "--runs") {
        config.runs = runs.parse().map_err(|e| format!("--runs: {e}"))?;
    }
    if let Some(warmup) = option(args, "--warmup") {
        config.warmup = warmup.parse().map_err(|e| format!("--warmup: {e}"))?;
    }
    let edges = list(option(args, "--edges"));
    if !edges.is_empty() {
        config.long_edges = edges
            .iter()
            .map(|e| e.parse().map_err(|e| format!("--edges: {e}")))
            .collect::<Result<_, String>>()?;
    }
    let wanted = list(option(args, "--backends"));
    let fixtures = FixtureSet::new(eval_dir());
    let mut rows: Vec<VisionBenchRow> = Vec::new();

    for spec in backends::local_specs() {
        if !wanted.is_empty() && !wanted.iter().any(|w| w == spec.name) {
            continue;
        }
        eprintln!("{}: loading", spec.name);
        let loaded = backends::load_local(spec.name).await.map_err(|e| e.to_string())?;
        let row = match loaded {
            Loaded::Agent(backend) => {
                let workload =
                    fixtures.workload(spec.workload, spec.scores).map_err(|e| e.to_string())?;
                run_backend(&config, &backend, &workload).await
            }
            Loaded::Embedder(embedder) => {
                let workload =
                    EmbedWorkload::captioned_fixtures(&fixtures).map_err(|e| e.to_string())?;
                run_embedder(
                    &config,
                    spec.name,
                    &embedder.runtime_label,
                    embedder.models,
                    Some(embedder.cold_load),
                    embedder.runtime,
                    &workload,
                )
                .await
            }
        }
        .map_err(|e| e.to_string())?;
        eprintln!("{}: {:?}", row.backend, row.accuracy);
        rows.push(row);
    }

    #[cfg(feature = "vision-cloud")]
    {
        if !args.iter().any(|a| a == "--no-recorded") {
            rows.extend(cloud::recorded(&config, &fixtures, &wanted).await?);
        }
        for provider in list(option(args, "--live")) {
            rows.push(cloud::live(&config, &fixtures, &provider).await?);
        }
    }

    let mut report = VisionBenchReport::new(runtime_versions(MachineInfo::detect()), rows);
    report.config = config;
    let markdown = report.to_markdown();
    match option(args, "--out") {
        Some(path) => std::fs::write(path, &markdown).map_err(|e| format!("{path}: {e}"))?,
        None => println!("{markdown}"),
    }
    if let Some(path) = option(args, "--json") {
        let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(())
}

async fn serve(name: Option<&str>) -> Result<(), String> {
    let name = name.ok_or("serve needs a backend name; see `list`")?;
    let Loaded::Agent(backend) = backends::load_local(name).await.map_err(|e| e.to_string())?
    else {
        return Err(format!("{name} is an embedder; eval sets score detect, segment and read"));
    };
    eprintln!("{name}: serving the cargo adk eval protocol on stdin and stdout");
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    serve_eval_protocol(&backend.agent, stdin, tokio::io::stdout()).await.map_err(|e| e.to_string())
}

#[cfg(feature = "vision-cloud")]
mod cloud {
    //! Cloud backends: recorded responses served to the real clients, and live calls.

    use std::sync::Arc;
    use std::time::Instant;

    use adk_bench::vision::backends::{CONCEPT_SCORES, LOCATE_SCORES};
    use adk_bench::vision::{
        FixtureSet, ModelRef, RunMode, VisionBackend, VisionBenchConfig, VisionBenchRow,
        run_backend,
    };
    use adk_core::Llm;
    use adk_core::egress::EgressPolicy;
    use adk_vision::eval::{EvalTask, VisionEvalAgent};
    use adk_vision::{
        EgressAction, EgressConfig, EgressGate, InMemoryEgressRecorder, LlmDescriber, SamConfig,
        SamSegmenter, VisionRuntime,
    };
    use serde_json::Value;
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate, matchers};

    /// The recorded rows: backend, recording stem, model id.
    pub const RECORDED: [&str; 6] = [
        "claude-sonnet-5-5",
        "claude-haiku-5-5",
        "nemotron-super-vl",
        "gemma-4",
        "muse-spark-1-3",
        "sam-3.1",
    ];

    fn gate() -> EgressGate {
        EgressGate::new(
            EgressPolicy::allow_private_networks(),
            EgressConfig::default().with_default(EgressAction::Allow),
            Arc::new(InMemoryEgressRecorder::new()),
        )
    }

    fn recording(fixtures: &FixtureSet, name: &str) -> Result<Value, String> {
        let path = fixtures.dir().join("../cloud/recorded").join(name);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn template(body: &Value) -> ResponseTemplate {
        match body {
            Value::String(text) => ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(text.clone()),
            other => ResponseTemplate::new(200).set_body_json(other),
        }
    }

    /// The describer for `backend`, its model id and licence, pointed at `base`.
    fn describer(backend: &str, base: &str, key: &str) -> Result<(LlmDescriber, String), String> {
        let fail = |e: &dyn std::fmt::Display| format!("{backend}: {e}");
        let (llm, hosts, model): (Arc<dyn Llm>, &str, &str) = match backend {
            "claude-sonnet-5-5" | "claude-haiku-5-5" => {
                let config = adk_model::anthropic::AnthropicConfig::new(key, backend)
                    .allow_insecure_http()
                    .with_base_url(base);
                let client = adk_model::AnthropicClient::new(config).map_err(|e| fail(&e))?;
                (Arc::new(client), "api.anthropic.com", backend)
            }
            "nemotron-super-vl" | "gemma-4" => {
                let model = if backend == "gemma-4" {
                    "google/gemma-4-31b-it"
                } else {
                    "nvidia/nemotron-3.5-super-vl-120b-a12b"
                };
                let config =
                    adk_vision::cloud::nim::NimConfig::new(format!("{base}/v1"), key, model);
                (
                    Arc::new(config.client().map_err(|e| fail(&e))?),
                    "integrate.api.nvidia.com",
                    model,
                )
            }
            "muse-spark-1-3" => {
                let config = adk_model::MuseConfig::new(key, "muse-spark-1.3")
                    .with_base_url(format!("{base}/v1"))
                    .with_max_tokens(4_000);
                (
                    Arc::new(adk_model::MuseClient::new(config).map_err(|e| fail(&e))?),
                    "api.meta.ai",
                    "muse-spark-1.3",
                )
            }
            other => return Err(format!("no describer named {other}")),
        };
        let mut describer = LlmDescriber::remote(llm, [hosts]).map_err(|e| fail(&e))?;
        describer = if backend.starts_with("claude") {
            describer.with_effort(adk_core::Effort::Low).with_max_output_tokens(4_000)
        } else if hosts == "integrate.api.nvidia.com" {
            describer.with_max_output_tokens(1_500).with_unpriced_hold(0)
        } else {
            describer
        };
        Ok((describer, model.to_string()))
    }

    fn describe_backend(
        id: &str,
        describer: LlmDescriber,
        model: &str,
        mode: RunMode,
    ) -> Result<VisionBackend, String> {
        let runtime = VisionRuntime::builder()
            .remote_describer(Arc::new(describer))
            .egress_gate(gate())
            .build()
            .map_err(|e| format!("{id}: {e}"))?;
        let runtime = Arc::new(runtime);
        let task = EvalTask::Describe { want_regions: true, schema: None };
        Ok(VisionBackend::new(id, "LlmDescriber", mode, |cx| {
            VisionEvalAgent::new(runtime, task).with_name(id).with_context(cx)
        })
        .with_models(vec![ModelRef {
            id: model.to_string(),
            revision: None,
            licence: "provider terms".to_string(),
        }]))
    }

    fn sam_backend(base: &str, key: &str, mode: RunMode) -> Result<VisionBackend, String> {
        let mut config = SamConfig::new(key);
        if !base.is_empty() {
            config = config.with_base_url(format!("{base}/v1"));
        }
        let sam = SamSegmenter::new(config).map_err(|e| format!("sam-3.1: {e}"))?;
        let runtime = VisionRuntime::builder()
            .remote_segmenter(Arc::new(sam.with_hosts(["api.meta.ai"])))
            .egress_gate(gate())
            .build()
            .map_err(|e| format!("sam-3.1: {e}"))?;
        let runtime = Arc::new(runtime);
        Ok(VisionBackend::new("sam-3.1", "SamSegmenter", mode, |cx| {
            VisionEvalAgent::new(runtime, EvalTask::Segment).with_name("sam-3.1").with_context(cx)
        })
        .with_models(vec![ModelRef {
            id: "sam-3.1".to_string(),
            revision: None,
            licence: "provider terms".to_string(),
        }]))
    }

    /// Serves each `sam-3.1` concept's recording by the phrase in its request.
    struct ByConcept(Vec<(String, Value)>);

    impl Respond for ByConcept {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body: Value = request.body_json().unwrap_or_default();
            let text = body["input"][0]["content"][0]["text"].as_str().unwrap_or_default();
            match self.0.iter().find(|(concept, _)| text.contains(concept.as_str())) {
                Some((_, recorded)) => template(&recorded["response"]),
                None => ResponseTemplate::new(404),
            }
        }
    }

    /// Every recorded cloud row whose backend `wanted` names (all when it is empty).
    pub async fn recorded(
        config: &VisionBenchConfig,
        fixtures: &FixtureSet,
        wanted: &[String],
    ) -> Result<Vec<VisionBenchRow>, String> {
        let mut rows = Vec::new();
        let picked = |name: &str| wanted.is_empty() || wanted.iter().any(|w| w == name);
        for backend in RECORDED.iter().copied().filter(|b| *b != "sam-3.1" && picked(b)) {
            for (workload, stem) in [("shapes-locate", "shapes"), ("s5-grounding", "s5-screen")] {
                let recorded = recording(fixtures, &format!("{backend}.{stem}.json"))?;
                let server = MockServer::start().await;
                Mock::given(matchers::method("POST"))
                    .and(matchers::path(recorded["path"].as_str().unwrap_or("/")))
                    .respond_with(template(&recorded["response"]))
                    .mount(&server)
                    .await;
                let (describer, model) = describer(backend, &server.uri(), "recorded")?;
                let backend = describe_backend(backend, describer, &model, RunMode::Recorded)?;
                let workload =
                    fixtures.workload(workload, LOCATE_SCORES).map_err(|e| e.to_string())?;
                let row =
                    run_backend(config, &backend, &workload).await.map_err(|e| e.to_string())?;
                eprintln!("{} on {}: {:?}", row.backend, row.workload, row.accuracy);
                rows.push(row);
            }
        }
        if picked("sam-3.1") {
            let concepts = ["red rectangle", "blue circle", "green triangle"]
                .into_iter()
                .map(|concept| {
                    let stem = format!("sam-3-1.shapes-{}.json", concept.replace(' ', "-"));
                    Ok((concept.to_string(), recording(fixtures, &stem)?))
                })
                .collect::<Result<Vec<_>, String>>()?;
            let server = MockServer::start().await;
            Mock::given(matchers::method("POST"))
                .and(matchers::path("/v1/responses"))
                .respond_with(ByConcept(concepts))
                .mount(&server)
                .await;
            let backend = sam_backend(&server.uri(), "recorded", RunMode::Recorded)?;
            let workload =
                fixtures.workload("shapes-concepts", CONCEPT_SCORES).map_err(|e| e.to_string())?;
            let row = run_backend(config, &backend, &workload).await.map_err(|e| e.to_string())?;
            eprintln!("{} on {}: {:?}", row.backend, row.workload, row.accuracy);
            rows.push(row);
        }
        Ok(rows)
    }

    fn key(var: &str) -> Result<String, String> {
        std::env::var(var)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| format!("{var} is not set"))
    }

    /// One live call: Claude or NVIDIA locating the drawn shapes, or `sam-3.1` segmenting the
    /// red rectangle.
    pub async fn live(
        config: &VisionBenchConfig,
        fixtures: &FixtureSet,
        provider: &str,
    ) -> Result<VisionBenchRow, String> {
        let (backend, mut workload) = match provider {
            "claude" => {
                let (describer, model) = describer(
                    "claude-sonnet-5-5",
                    "https://api.anthropic.com",
                    &key("ANTHROPIC_API_KEY")?,
                )?;
                let workload =
                    fixtures.workload("shapes-locate", LOCATE_SCORES).map_err(|e| e.to_string())?;
                (describe_backend("claude-sonnet-5-5", describer, &model, RunMode::Live)?, workload)
            }
            "nvidia" => {
                let (describer, model) = describer(
                    "nemotron-super-vl",
                    "https://integrate.api.nvidia.com",
                    &key("NVIDIA_API_KEY")?,
                )?;
                let workload =
                    fixtures.workload("shapes-locate", LOCATE_SCORES).map_err(|e| e.to_string())?;
                (describe_backend("nemotron-super-vl", describer, &model, RunMode::Live)?, workload)
            }
            "sam" => {
                let mut workload = fixtures
                    .workload("shapes-concepts", CONCEPT_SCORES)
                    .map_err(|e| e.to_string())?;
                workload.eval_set.eval_cases.truncate(1);
                (sam_backend("", &key("META_API_KEY")?, RunMode::Live)?, workload)
            }
            other => {
                return Err(format!("no live provider named {other}; use claude, nvidia or sam"));
            }
        };
        workload.latency = None;
        let started = Instant::now();
        let mut row = run_backend(config, &backend, &workload).await.map_err(|e| e.to_string())?;
        let wall = started.elapsed();
        row.notes.push(format!("one live call, {:.2} s wall", wall.as_secs_f64()));
        eprintln!("{} live on {}: {:?} in {wall:?}", row.backend, row.workload, row.accuracy);
        Ok(row)
    }
}
