//! The `autonomous` template: an agent that works toward a goal unattended and wakes itself
//! on a schedule, on the umbrella's `autonomy` feature
//! (`docs/official_docs/autonomy/index.md`).
//!
//! Everything it must not forget is in one SQLite file, its one side effect waits in an
//! approvals inbox, each run has a budget and each day a spend cap, every tool decision is on
//! a signed audit log, and `bash` runs in the OS sandbox where the platform has one.

use crate::template::{AgentCodeFragments, AgentTemplate, FileFragment, TemplateCategory};

/// The `autonomous` template.
pub fn template() -> AgentTemplate {
    AgentTemplate {
        name: "autonomous",
        description: "Autonomous agent: rubric-graded goals and a schedule on the agent kernel, durable SQLite stores, an approval-gated side effect, run budget and spend cap, signed audit, OS sandbox",
        category: TemplateCategory::AgentType,
        default_provider: "gemini",
        required_features: vec!["minimal", "autonomy", "cli"],
        // The launcher serves the kernel itself (`cargo run -- up`), on loopback only.
        incompatible_addons: vec!["server", "docker"],
        additional_deps: vec![],
        code_fragments: AgentCodeFragments {
            // `Arc` and `HarnessAgent` come from the prelude.
            imports: vec![
                "mod audit;",
                "mod publish;",
                "use adk_rust::harness::{",
                "    ConsoleIo, ConsolePrompter, HarnessConfig, HarnessLauncher, HarnessMode, SqliteGoalStore,",
                "    SqliteModeStore, SqliteStandingRuleStore, Workspace,",
                "};",
                "use adk_rust::kernel::stores::KernelStores;",
                "use adk_rust::kernel::{KernelConfig, Quotas, Trigger, Wake, WakeOverlap};",
                "use adk_rust::session::SqliteSessionService;",
                "use adk_rust::{MissedFirePolicy, NewRun, RunBudget, StoreApprovalService};",
            ],
            agent_construction: AGENT_CONSTRUCTION,
            additional_files: vec![
                FileFragment { path: "src/publish.rs", content: PUBLISH_RS },
                FileFragment { path: "src/audit.rs", content: AUDIT_RS },
                FileFragment { path: ".adk/harness.toml", content: HARNESS_TOML },
                FileFragment { path: "rubric.md", content: RUBRIC },
                FileFragment { path: "notes/example.md", content: EXAMPLE_NOTE },
                FileFragment { path: ".gitignore", content: GITIGNORE },
                // Replaces the generic README with the autonomous agent's commands.
                FileFragment { path: "README.md", content: README },
            ],
        },
    }
}

/// The agent, its durable stores and the launcher; `model` comes from the provider.
const AGENT_CONSTRUCTION: &str = r#"// Sessions belong to you, so `adk-rust attach` and steering over the kernel's socket,
    // which admit only your OS user, reach them.
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".to_string());

    // One SQLite file keeps what must survive a crash: sessions, durable runs, held calls and
    // decisions, the action ledger, spend, standing rules, modes, goals, the signed audit log,
    // and the kernel's processes, schedules and kill switch.
    std::fs::create_dir_all(".adk")?;
    let sessions = Arc::new(SqliteSessionService::new("sqlite://.adk/state.db?mode=rwc").await?);
    sessions.migrate().await?;
    let pool = sessions.pool().clone();
    let stores = KernelStores::from_sqlite_pool(pool.clone()).await?;
    let rules = Arc::new(SqliteStandingRuleStore::new(pool.clone()));
    rules.migrate().await?;
    let modes = Arc::new(SqliteModeStore::new(pool.clone()));
    modes.migrate().await?;
    let goals = Arc::new(SqliteGoalStore::new(pool.clone()));
    goals.migrate().await?;

    // A held call waits in one approvals inbox; a decision there resumes the parked run.
    let approvals = Arc::new(
        StoreApprovalService::new(stores.approvals.clone()).with_run_store(stores.runs.clone()),
    );

    // One terminal reader serves the REPL and the approval prompts of runs in this process;
    // runs on the kernel park their held calls in the inbox instead.
    let console = ConsoleIo::terminal()?;
    let harness = HarnessAgent::builder("{name}")
        .model(Arc::new(model))
        .instruction(
            "You keep STATUS.md, a short status report of this project, up to date from the \
             notes in notes/. Read the notes, write STATUS.md, then publish it with \
             publish_status. Publish only when STATUS.md changed.",
        )
        .workspace(Workspace::new(".").allow_bash(true))
        // Auto: reads, edits and sandboxed commands run; publish_status waits for approval.
        .mode(HarnessMode::Auto)
        .config(HarnessConfig::discover(".")?) // .adk/harness.toml: the OS sandbox
        .tool(publish::tool("outbox"))
        .prompter(Arc::new(ConsolePrompter::new(console.clone())))
        .approval_service(approvals)
        .action_ledger(stores.actions.clone())
        .standing_rules(rules)
        .mode_store(modes)
        .goal_store(goals)
        .spend_ledger(stores.spend.clone())
        // Each run stops at 40 model calls or 0.50 USD, whichever comes first.
        .budget(RunBudget::new().max_model_calls(40).max_cost_usd(0.50))
        .audit_log(audit::open(&sessions, "{name}").await?)
        .build()?;

    // Each weekday at 09:00 UTC the kernel starts a run with this message. Six fields,
    // seconds first; the 09:00s missed while the kernel was down or frozen fire once.
    let weekdays = Trigger::new(
        "weekday-status",
        Wake::Cron { expr: "0 0 9 * * Mon-Fri".into(), missed: MissedFirePolicy::FireOnce },
        NewRun::new(
            "background",
            "{name}",
            user.clone(),
            "{name}",
            adk_rust::serde_json::json!({ "text": "Refresh STATUS.md from notes/ and publish it if it changed." }),
        ),
    )
    // While a run waits for approval, later weekdays start nothing rather than queue behind it.
    .with_overlap(WakeOverlap::Skip);

    // `cargo run` is the REPL; `cargo run -- up` runs the agent kernel with its home in .adk;
    // `cargo run -- goal "…" --until "…"` works toward a goal, on that kernel when it runs,
    // and a separate grader checks each iteration against rubric.md ([goal] in
    // .adk/harness.toml).
    HarnessLauncher::new(harness)
        .with_user_id(user)
        .with_session_service(sessions)
        .with_console(console)
        .with_kernel_stores(stores)
        .with_kernel_config(
            KernelConfig::default()
                .with_home(".adk")
                // Background work waits for the next UTC day once the day's spend reaches 2 USD.
                .with_quotas(Quotas::new().with_daily_spend_cap(2_000_000)),
        )
        .with_trigger(weekdays)
        .run()
        .await?;"#;

/// `src/publish.rs`: the agent's one side effect, and the test that it waits for approval.
const PUBLISH_RS: &str = r##"//! The agent's one side effect: publishing STATUS.md.
//!
//! Here it appends to `outbox/published.md`; in your agent it posts to a chat channel, sends
//! an email or opens a ticket. The harness governs it as an external tool, so in every mode
//! a call waits for a person's decision, and as `NonIdempotent` the action ledger keeps a
//! call in flight at a crash from running twice.

use std::io::Write;
use std::path::PathBuf;

use adk_rust::prelude::*;
use adk_rust::serde_json::{Value, json};
use adk_rust::tool::FunctionTool;
use adk_rust::{AdkError, ToolEffect};

/// `publish_status`, appending STATUS.md to `published.md` in `outbox`.
pub fn tool(outbox: impl Into<PathBuf>) -> Arc<dyn Tool> {
    let outbox = outbox.into();
    let tool = FunctionTool::new(
        "publish_status",
        "Publishes STATUS.md to the team. Call it once STATUS.md is written.",
        move |_ctx, _args: Value| {
            let outbox = outbox.clone();
            async move {
                let status = std::fs::read_to_string("STATUS.md")
                    .map_err(|e| AdkError::tool(format!("STATUS.md cannot be read: {e}")))?;
                std::fs::create_dir_all(&outbox).map_err(|e| AdkError::tool(e.to_string()))?;
                let mut published = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(outbox.join("published.md"))
                    .map_err(|e| AdkError::tool(e.to_string()))?;
                writeln!(published, "{status}\n---").map_err(|e| AdkError::tool(e.to_string()))?;
                Ok(json!({ "published": true, "bytes": status.len() }))
            }
        },
    )
    .with_effect(ToolEffect::NonIdempotent);
    Arc::new(tool)
}

#[cfg(test)]
mod tests {
    use adk_rust::futures::StreamExt;
    use adk_rust::harness::{HarnessAgent, HarnessMode, InstructionsConfig, Workspace};
    use adk_rust::model::{ScriptedLlm, ScriptedResponse};
    use adk_rust::UserId;
    use adk_rust::session::InMemorySessionService;

    use super::*;

    /// A scripted model plays the agent, so `cargo test` needs no API key.
    #[tokio::test]
    async fn publishing_waits_for_a_decision_in_auto_mode() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!("{name}-publish-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let model = ScriptedLlm::new(
            "scripted",
            vec![
                ScriptedResponse::function_call("publish_status", json!({})),
                ScriptedResponse::text("Held."),
            ],
        );
        let harness = HarnessAgent::builder("{name}")
            .model(Arc::new(model))
            .workspace(Workspace::new(&dir))
            .project_instructions(InstructionsConfig { user_file: None, ..InstructionsConfig::default() })
            .mode(HarnessMode::Auto)
            .tool(tool(dir.join("outbox")))
            .build()?
            .into_harness("{name}", Arc::new(InMemorySessionService::new()))?;

        let user = UserId::new("user")?;
        let session = harness.open_session(&user, None).await?;
        let mut events = harness.send(&user, &session, "publish the status").await?;
        while let Some(event) = events.next().await {
            event?;
        }

        // Nothing was published; the call waits for a person, as the inbox would show it.
        assert!(!dir.join("outbox/published.md").exists(), "published without a decision");
        let pending = harness.pending_approvals(&user, &session).await?;
        let held: Vec<&str> =
            pending.iter().map(|entry| entry.request.tool_name.as_str()).collect();
        assert_eq!(held, ["publish_status"]);
        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }
}
"##;

/// `src/audit.rs`: the signed audit log and its key.
const AUDIT_RS: &str = r##"//! The signed audit log: every tool decision, spend and outcome on a hash chain in
//! `.adk/state.db`, signed with a key kept in `.adk/audit.key`.
//!
//! Verify it with `adk-rust audit export --database-url sqlite://.adk/state.db --log-id {name}
//! --out audit.jsonl` and `adk-rust audit verify audit.jsonl --key "$(cat .adk/audit.vkey)"`.

use std::path::Path;

use adk_rust::auth::signed_audit::{AuditLog, AuditSigner, Ed25519Signer};
use adk_rust::audit_log::AuditRecorder;
use adk_rust::prelude::*;
use adk_rust::session::{SqliteAuditLogStore, SqliteSessionService};

/// The signer key, written once, readable by its owner only.
const KEY: &str = ".adk/audit.key";
/// The verifier key `adk-rust audit verify` takes.
const VERIFIER: &str = ".adk/audit.vkey";

/// Opens the log `log_id` in the sessions' database, creating the signer key on the first
/// run.
pub async fn open(
    sessions: &SqliteSessionService,
    log_id: &str,
) -> anyhow::Result<Arc<dyn AuditRecorder>> {
    let store = SqliteAuditLogStore::from_pool(sessions.pool().clone());
    store.migrate().await?;
    let signer = match std::fs::read_to_string(KEY) {
        Ok(key) => Ed25519Signer::from_note_key(key.trim())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let signer = Ed25519Signer::generate(log_id)?;
            write_private(Path::new(KEY), signer.to_note_key().as_str())?;
            std::fs::write(VERIFIER, signer.verifier_key())?;
            signer
        }
        Err(error) => return Err(error.into()),
    };
    let log: Arc<dyn AuditRecorder> =
        Arc::new(AuditLog::builder(Arc::new(store), log_id, Arc::new(signer)).open().await?);
    Ok(log)
}

#[cfg(unix)]
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    file.write_all(text.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, text)
}
"##;

/// `.adk/harness.toml`: the OS sandbox for `bash`.
const HARNESS_TOML: &str = r##"# Harness settings for this project, read by `HarnessConfig::discover` in src/main.rs.
# This file can only tighten what src/main.rs sets.

# Run bash in the OS sandbox: Seatbelt on macOS, bubblewrap on Linux. Where neither exists,
# bash runs on the host and every command waits for approval.
sandbox = true

# Refuse to start where the sandbox is unavailable, instead of the host behind approvals.
# fail_if_unavailable = true

# `cargo run -- goal` grades each iteration against rubric.md: a separate grader reads only
# STATUS.md and the agent's final message, and the goal is met once every criterion passes
# (and its --until command, when given). Its feedback drives the next iteration; after
# `max_evaluations` failed rounds the goal stops as exhausted. The grader's calls count
# against the run budget in src/main.rs.
[goal]
rubric = "rubric.md"
deliverables = ["STATUS.md"]
max_evaluations = 3
"##;

/// `rubric.md`: what a finished STATUS.md looks like, one gradeable criterion per item.
const RUBRIC: &str = r##"# STATUS.md rubric

Each item is graded pass or fail on its own, from STATUS.md alone.

- STATUS.md starts with a level-one heading that contains the word "Status".
- It has a section that lists open risks or problems, and names an owner for each one.
- It has a section that lists upcoming work, and gives a day for each scheduled item.
- It is at most 150 words long.
"##;

/// `notes/example.md`: something for the agent to report on.
const EXAMPLE_NOTE: &str = r##"# Notes

- The release candidate passed the integration suite.
- Two flaky tests in the payments module are quarantined; owner: Ana.
- The staging database migration is scheduled for Thursday.
"##;

/// `.gitignore`: the base entries and the agent's state.
const GITIGNORE: &str = r##"/target
.env
*.swp
*.swo
*~
.DS_Store
# The agent's state and keys (src/main.rs, src/audit.rs)
.adk/state.db*
.adk/audit.key
.adk/kernel.sock
"##;

/// The project README.
const README: &str = r##"# {name}

An autonomous agent built on `adk-rust`'s `autonomy` feature. It keeps `STATUS.md` up to date
from the notes in `notes/` and publishes it, working toward a goal unattended and waking
itself each weekday.

| Property | How |
|----------|-----|
| Durable | Sessions, runs, held calls, the action ledger, spend, rules, modes, goals, the audit log and the kernel's processes in `.adk/state.db` |
| Graded | A separate grader checks each goal iteration against `rubric.md` and feeds its findings back |
| Governed | `auto` mode: reads, edits and sandboxed commands run; `publish_status` waits for approval |
| Bounded | 40 model calls or 0.50 USD per run; background work stops at 2 USD a day |
| Sandboxed | `bash` in Seatbelt (macOS) or bubblewrap (Linux), set in `.adk/harness.toml` |
| Audited | Every tool decision on a signed hash chain; the key is `.adk/audit.key` |
| Scheduled | A cron trigger at 09:00 UTC on weekdays |

## Run it

```bash
cp .env.example .env            # add your API key
cargo test                      # the scripted test: publishing waits for a decision
cargo run -- up                 # terminal 1: the agent kernel, home .adk
cargo run -- goal "Write STATUS.md from notes/ and publish it" --until "test -s outbox/published.md"
adk-rust approvals --home .adk list           # terminal 3: the held publish_status call
adk-rust approvals --home .adk approve ID     # the goal resumes on the kernel
```

| Command | Does |
|---------|------|
| `cargo run` | The REPL; approvals are asked in the terminal |
| `cargo run -- up` | The agent kernel: goals, the weekday trigger and their held calls run here |
| `cargo run -- goal TEXT --until CMD` | Works toward `TEXT` until `CMD` exits 0 and the grader passes `rubric.md`, on the kernel when it runs, else here |
| `cargo run -- goal --resume` | Continues the latest unfinished goal |
| `adk-rust ps --home .adk` | The kernel's processes |
| `adk-rust approvals --home .adk list` | Calls waiting for a decision |
| `adk-rust freeze --home .adk` / `unfreeze` | The kill switch: nothing runs until unfrozen |
| `adk-rust events --home .adk --follow` | The kernel's event bus |

`adk-rust` is the ADK-Rust CLI: `cargo install adk-cli`.

## Files

| File | Purpose |
|------|---------|
| `src/main.rs` | The stores, the agent, the weekday trigger and the launcher |
| `src/publish.rs` | `publish_status`, the side effect, and its test |
| `src/audit.rs` | The signed audit log and its key |
| `.adk/harness.toml` | The OS sandbox and the goal rubric; it can only tighten `src/main.rs` |
| `rubric.md` | What a finished `STATUS.md` looks like; one criterion per list item |
| `notes/` | What the agent reports on |
| `.adk/state.db` | Everything durable, created on the first run (git-ignored) |

## Resources

- [Build an autonomous agent](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/autonomy/index.md)
- [Harness documentation](https://github.com/zavora-ai/adk-rust/tree/main/docs/official_docs/harness)
- [Agent kernel documentation](https://github.com/zavora-ai/adk-rust/tree/main/docs/official_docs/kernel)
"##;
