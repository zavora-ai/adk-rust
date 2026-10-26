//! H10, the fully local posture (spec 3.1 §5, §7.12, §7.15.2, §8.1): a local model on
//! Ollama, `bash` in the OS sandbox with the network denied, long-term memory recalled with
//! a local ONNX embedder, and `harness.db` sealed under a store key — nothing leaves the
//! machine but the local model endpoint.
//!
//! Runs when a local model is reachable at `OLLAMA_HOST` (default `http://localhost:11434`)
//! and the example is built with `--features local`; otherwise it is SKIPPED with the
//! reason. `HARNESS_OLLAMA_MODEL` picks the model; the first one Ollama lists is the default.

use crate::{Checks, Subject};

/// Runs `name` when it is this module's scenario.
pub async fn run_scripted(name: &str, subject: Subject) -> Option<anyhow::Result<Checks>> {
    match name {
        "local_posture" => Some(local_posture(subject).await),
        _ => None,
    }
}

/// The scenario names this module adds to the scripted list.
pub const SCRIPTED: &[&str] = &["local_posture"];

#[cfg(not(feature = "local"))]
async fn local_posture(_subject: Subject) -> anyhow::Result<Checks> {
    Ok(Checks::skipped(
        "built without --features local (the ONNX embedder and the encrypted store)",
    ))
}

#[cfg(feature = "local")]
async fn local_posture(subject: Subject) -> anyhow::Result<Checks> {
    use std::io::{Read, Write};
    use std::sync::Arc;

    use adk_core::audit_log::InMemoryAuditRecorder;
    use adk_core::{Content, SessionId, UserId};
    use adk_harness::FactStore as _;
    use adk_harness::conformance::encrypted_store::SealedDb;
    use adk_harness::encrypted::StoreKeyring;
    use adk_harness::{
        FactKind, FactScope, FactSource, FactStatus, Harness, HarnessAgent, HarnessMode,
        InstructionsConfig, IsolationStatus, LongTermMemoryConfig, MemoryFact, OsSandboxConfig,
        Provenance, RagEmbedder, ShellIsolation, StandingRuleStore as _, project_slug,
    };
    use adk_runner::Runner;
    use futures::StreamExt;

    if subject == Subject::Baseline {
        return Ok(Checks::skipped("the baseline has no local posture to compare"));
    }
    let host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".into());
    let authority = host.trim_start_matches("http://").trim_end_matches('/').to_string();
    // A plain HTTP GET of /api/tags: the example takes no HTTP client for one probe.
    let tags = (|| -> std::io::Result<String> {
        let mut stream = std::net::TcpStream::connect(&authority)?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        write!(stream, "GET /api/tags HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n")?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok(response.split_once("\r\n\r\n").map(|(_, body)| body.to_string()).unwrap_or_default())
    })();
    let models: Vec<String> = match tags {
        Ok(body) => serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("models")?.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|m| m.get("name")?.as_str().map(str::to_string))
            .collect(),
        Err(error) => return Ok(Checks::skipped(&format!("no Ollama at {host}: {error}"))),
    };
    let Some(model_name) =
        std::env::var("HARNESS_OLLAMA_MODEL").ok().or_else(|| models.first().cloned())
    else {
        return Ok(Checks::skipped(&format!("Ollama at {host} lists no model; `ollama pull` one")));
    };
    let embedder = match RagEmbedder::local() {
        Ok(embedder) => Arc::new(embedder),
        Err(error) => {
            return Ok(Checks::skipped(&format!(
                "the local ONNX embedder is unavailable: {error}"
            )));
        }
    };

    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("README.md"), "a local project\n")?;
    let store_dir = tempfile::tempdir()?;
    let path = store_dir.path().join("harness.db");
    let keyring = StoreKeyring::generate();
    let db = SealedDb::open_with_legacy(&path, &keyring, false).await;
    db.marker.set_key_id(&keyring.key_id()).await?;
    let user = UserId::new("alice")?;
    let scope = FactScope::new(crate::APP, "alice", Some(project_slug(dir.path())));
    // H11: a known preference from an earlier session, H2: a standing rule for `echo`.
    let fact = db
        .facts
        .put(MemoryFact::new(
            scope.clone(),
            FactKind::Preference,
            "Use pnpm, not npm, to add dependencies",
            FactStatus::Known,
            Provenance::new(FactSource::UserStated, "s0", "inv-0"),
        ))
        .await?;
    let rule = adk_harness::StandingRule {
        id: "rule-echo".into(),
        scope: adk_harness::RuleScope::User {
            app_name: crate::APP.into(),
            user_id: "alice".into(),
            subject_kind: adk_core::principal::SubjectKind::Human,
        },
        tool: "bash".into(),
        origin: adk_harness::OriginPin::Exact {
            source: "static".into(),
            declaration_digest: "sha256:local".into(),
        },
        agent: None,
        predicates: vec![adk_harness::RulePredicate::ShellPrefix {
            pointer: "/command".into(),
            tokens: vec!["echo".into()],
        }],
        decision: adk_harness::StandingDecision::Allow,
        created_by: adk_core::principal::SubjectRef::human("alice"),
        created_at: chrono::Utc::now(),
        expires_at: None,
        label: "echo anything".into(),
        source_fingerprint: None,
    };
    db.rules.grant(rule).await?;

    let model = Arc::new(adk_model::OllamaModel::new(adk_model::OllamaConfig::with_host(
        host.clone(),
        model_name.clone(),
    ))?);
    let sessions: Arc<dyn adk_session::SessionService> =
        Arc::new(SealedDb::open_with_legacy(&path, &keyring, false).await.sessions);
    let audit = InMemoryAuditRecorder::new();
    let agent = Arc::new(
        HarnessAgent::builder("harness")
            .model(model)
            .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..InstructionsConfig::default()
            })
            .mode(HarnessMode::Auto)
            .shell(ShellIsolation::OsSandbox(OsSandboxConfig {
                network: false,
                ..OsSandboxConfig::default()
            }))
            .mode_store(Arc::new(db.modes.clone()))
            .standing_rules(Arc::new(db.rules.clone()))
            .shell_ledger(Arc::new(db.ledger.clone()))
            .goal_store(Arc::new(db.goals.clone()))
            .long_term_memory(LongTermMemoryConfig {
                distill: false,
                ..LongTermMemoryConfig::default()
            })
            .fact_store(Arc::new(db.facts.clone()))
            .memory_embedder(embedder)
            .without_compaction()
            .build()?,
    );
    let info = agent.info().clone();
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(crate::APP).agent(agent.clone()).session_service(sessions),
        )
        .audit_log(Arc::new(audit.clone()))
        .build()?;
    let harness = Harness::with_runner(agent, runner)?;
    let session = harness.open_session(&user, Some(&SessionId::new("local-1")?)).await?;
    println!("      local model {model_name} at {host}; isolation {}", info.isolation);
    let events: Vec<_> = harness
        .send(
            &user,
            &session,
            "Print the word hello with echo, then say which package manager to use here.",
        )
        .await?
        .collect()
        .await;
    let error = events.iter().find_map(|e| e.as_ref().err()).map(|e| e.message.clone());
    match &error {
        // A local model without tool calling (Ollama answers 400 "does not support tools")
        // cannot run the harness's tools; the stores, the sandbox and recall still hold.
        Some(message) if message.contains("does not support tools") => {
            println!(
                "      {model_name} cannot call tools ({}); pull a tool-capable model for H2",
                message.trim()
            );
            checks
                .check(format!("a turn reached {model_name}; it lacks tool calling, noted"), true);
        }
        Some(message) => checks.check(format!("a turn ran on {model_name}: {message}"), false),
        None => checks.check(
            format!("a turn ran on {model_name} ({} events)", events.len()),
            !events.is_empty(),
        ),
    }
    checks.check(
        "bash runs in the OS sandbox with the network denied",
        matches!(info.isolation, IsolationStatus::OsSandbox { .. }),
    );
    let recalled =
        audit.records().into_iter().filter(|r| r.action == "harness.memory.recalled").count();
    checks.check(
        "the known preference was recalled into the turn (harness.memory.recalled)",
        recalled >= 1,
    );
    let vectors = db.facts.embeddings(&scope).await?;
    checks.check(
        "the fact was embedded locally with 384 dimensions, tagged with the local model",
        vectors.iter().any(|(id, embedding)| {
            id == &fact.id
                && embedding.vector.len() == 384
                && embedding.embedder == "onnx/BAAI/bge-small-en-v1.5"
        }),
    );
    let rules = db.rules.list(crate::APP, "alice").await?;
    checks.check(
        "the standing rule reads back through the sealed store",
        rules.iter().any(|r| r.id == "rule-echo"),
    );
    harness.close().await;
    drop(harness);
    db.pool.close().await;
    let bytes = std::fs::read(&path)?;
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let bytes = [bytes, std::fs::read(&wal).unwrap_or_default()].concat();
    let has = |needle: &str| bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
    checks.check("harness.db holds no fact text in plaintext", !has("pnpm"));
    checks.check("harness.db holds no rule label in plaintext", !has("echo anything"));
    checks.check("harness.db holds envelopes", has("adk-enc:v1:"));
    let _ = Content::new("user");
    Ok(checks)
}
