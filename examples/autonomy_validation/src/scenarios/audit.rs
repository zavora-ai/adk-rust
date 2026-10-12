//! `audit_trail`: a live run writes a signed audit log. One turn makes an allowed lookup,
//! a call the policy denies, and a purchase through the `adk-payments` tools; the runner's
//! audit log records each decision and the payment in a SQLite store, and the exported log
//! verifies with `adk-rust audit verify` until one record is edited.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::LlmAgentBuilder;
use adk_auth::signed_audit::{AuditExport, AuditLog, AuditSigner, Ed25519Signer, NoteVerifier};
use adk_core::{
    DeclarativePolicy, Llm, Result as AdkResult, Tool, ToolContext, ToolEffect, async_trait,
};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService, SqliteAuditLogStore};
use serde_json::{Value, json};

use crate::common::{Config, CountingTool, Provider, Verdict, brief, fail, run_turn};
use crate::scenarios::commerce::{MemoryMerchant, buy_from};

const APP: &str = "autonomy-audit";
const USER: &str = "user-audit";
const SESSION: &str = "audit-trail";
const AGENT: &str = "account_agent";
const LOG_ID: &str = "audit/replica-1";
/// Appears only in tool arguments; no record may contain it.
const SECRET_CUSTOMER: &str = "C-SECRET-7731";

/// Buys API credits through the payment tools, as the calling agent and user.
struct BuyCredits {
    merchant: Arc<MemoryMerchant>,
    executions: AtomicUsize,
}

#[async_trait]
impl Tool for BuyCredits {
    fn name(&self) -> &str {
        "buy_credits"
    }

    fn description(&self) -> &str {
        "Buys API credits for the customer's account. Pays the merchant with the stored card."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "cents": { "type": "integer", "description": "Amount in US cents" } },
            "required": ["cents"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        let cents = args["cents"].as_i64().unwrap_or(100).clamp(1, 500);
        let receipt = buy_from(&self.merchant, ctx.as_ref(), cents).await?;
        Ok(json!({ "paid_cents": cents, "status": receipt["status"] }))
    }
}

pub async fn audit_trail(cfg: &Config, provider: Provider) -> Verdict {
    match run(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

/// `(kind, action, decision, outcome)` of one exported record.
type Line = (String, String, Option<String>, Option<String>);

async fn run(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    let dir = tempfile::tempdir()?;
    let database = format!("sqlite://{}?mode=rwc", dir.path().join("audit.db").display());
    let store = Arc::new(SqliteAuditLogStore::new(&database).await?);
    store.migrate().await?;
    let signer = Arc::new(Ed25519Signer::generate("audit.autonomy.test")?);
    let verifier_key = signer.verifier_key();
    let log = Arc::new(AuditLog::builder(store.clone(), LOG_ID, signer).open().await?);

    let lookup = CountingTool::new(
        "lookup_account",
        "Looks up a customer account.",
        json!({ "type": "object", "properties": { "customer_id": { "type": "string" } }, "required": ["customer_id"] }),
        |_, args| Ok(json!({ "customer_id": args["customer_id"], "plan": "pro" })),
    );
    let delete = CountingTool::new(
        "delete_account",
        "Deletes a customer account and all its data.",
        json!({ "type": "object", "properties": { "customer_id": { "type": "string" } }, "required": ["customer_id"] }),
        |_, _| Ok(json!({ "deleted": true })),
    );
    let merchant = Arc::new(MemoryMerchant::default());
    let buy = Arc::new(BuyCredits { merchant: merchant.clone(), executions: AtomicUsize::new(0) });
    let agent = LlmAgentBuilder::new(AGENT)
        .model(cfg.model(provider)? as Arc<dyn Llm>)
        .instruction(
            "You are an account agent. Make exactly the tool calls the user lists, one at a \
             time, in order, even when an earlier call is denied or fails. Never repeat a call. \
             Then report each result in one line.",
        )
        .tool(lookup.clone() as Arc<dyn Tool>)
        .tool(delete.clone() as Arc<dyn Tool>)
        .tool(buy.clone() as Arc<dyn Tool>)
        .build()?;
    let policy = DeclarativePolicy::builder()
        .name("audit-validation-v1")
        .allow("lookup_account")
        .allow("buy_credits")
        .build();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(adk_session::CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: Default::default(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .tool_policy(Arc::new(policy))
        .audit_log(log.clone())
        .build()?;

    let turn = run_turn(
        &runner,
        USER,
        SESSION,
        &format!(
            "Make these three tool calls one at a time, in this order:\n\
             1. lookup_account with customer_id \"{SECRET_CUSTOMER}\"\n\
             2. delete_account with customer_id \"{SECRET_CUSTOMER}\"\n\
             3. buy_credits with cents 150\n\
             Then report each result."
        ),
    )
    .await;
    if let Some(error) = &turn.error {
        return Ok(Verdict::Fail(format!("turn failed: {}", brief(error))));
    }
    if delete.executions() != 0 {
        return Ok(Verdict::Fail("the denied delete_account executed".to_string()));
    }
    if lookup.executions() != 1 || buy.executions.load(Ordering::SeqCst) != 1 {
        return Ok(Verdict::Retry(format!(
            "model did not make each allowed call once (lookup {}x, buy {}x)",
            lookup.executions(),
            buy.executions.load(Ordering::SeqCst)
        )));
    }
    if merchant.completions() != 1 {
        return Ok(Verdict::Fail(format!(
            "{} payments completed, expected 1",
            merchant.completions()
        )));
    }
    log.checkpoint().await?;

    let export = AuditExport::from_store(store.as_ref(), LOG_ID).await?;
    let mut lines: Vec<Line> = Vec::new();
    let agent_id = format!("urn:adk:agent:{APP}/{AGENT}");
    for entry in &export.entries {
        let text = entry.record.as_deref().unwrap_or_default();
        if text.contains(SECRET_CUSTOMER) {
            return Ok(Verdict::Fail(format!("record {} carries an argument", entry.seq)));
        }
        let record: Value = serde_json::from_str(text)?;
        if record["actor"]["agent"] != json!(agent_id)
            || record["actor"]["onBehalfOf"]["id"] != json!(USER)
        {
            return Ok(Verdict::Fail(format!(
                "record {} names the wrong actor: {}",
                entry.seq, record["actor"]
            )));
        }
        let field = |name: &str| record[name].as_str().map(str::to_string);
        lines.push((
            field("kind").unwrap_or_default(),
            field("action").unwrap_or_default(),
            field("decision"),
            field("outcome"),
        ));
    }
    let expected = |kind: &str, action: &str, decision: Option<&str>, outcome: Option<&str>| {
        (
            kind.to_string(),
            action.to_string(),
            decision.map(str::to_string),
            outcome.map(str::to_string),
        )
    };
    let wanted = [
        expected("tool_call", "lookup_account", Some("allowed"), None),
        expected("tool_call", "delete_account", Some("denied"), Some("not_run")),
        expected("tool_call", "buy_credits", Some("allowed"), None),
        expected("payment", "payment.create_checkout", Some("allowed"), None),
        expected("payment", "payment.create_checkout", None, Some("succeeded")),
        expected("payment", "payment.complete_checkout", Some("allowed"), None),
        expected("payment", "payment.complete_checkout", None, Some("succeeded")),
    ];
    if lines != wanted {
        return Ok(Verdict::Fail(format!("records {lines:?}, expected {wanted:?}")));
    }

    // Verify the export, then the same export with the denial edited into an allowance.
    let verified = verify(&export, &verifier_key, dir.path(), "audit.jsonl")?;
    if !verified.accepted
        || !brief(&verified.report).contains(&format!("signed through: {}", lines.len()))
    {
        return Ok(Verdict::Fail(format!(
            "the log does not verify with every record signed: {}",
            verified.report
        )));
    }
    let mut tampered = export.clone();
    let denied = tampered.entries[1].record.take().unwrap_or_default();
    tampered.entries[1].record = Some(denied.replace("\"denied\"", "\"allowed\""));
    let rejected = verify(&tampered, &verifier_key, dir.path(), "tampered.jsonl")?;
    if rejected.accepted {
        return Ok(Verdict::Fail("an edited record still verifies".to_string()));
    }

    Ok(Verdict::Pass(format!(
        "{} records (allowed, denied, allowed, 2 payments x decision+outcome), actor {agent_id} for {USER}, digests only; {} accepts the log ({}) and rejects it with record 1 edited ({})",
        lines.len(),
        verified.by,
        brief(&verified.report),
        brief(&rejected.report),
    )))
}

struct Verification {
    accepted: bool,
    report: String,
    by: &'static str,
}

/// Verifies `export` with `adk-rust audit verify` when `ADK_RUST_BIN` names the CLI,
/// and with the library's `verify` otherwise.
fn verify(
    export: &AuditExport,
    verifier_key: &str,
    dir: &Path,
    file: &str,
) -> anyhow::Result<Verification> {
    let path = dir.join(file);
    std::fs::write(&path, export.to_jsonl())?;
    match std::env::var("ADK_RUST_BIN") {
        Ok(bin) => {
            let output = Command::new(bin)
                .args(["audit", "verify"])
                .arg(&path)
                .args(["--key", verifier_key])
                .output()?;
            let report = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(Verification {
                accepted: output.status.success(),
                report,
                by: "adk-rust audit verify",
            })
        }
        Err(_) => {
            let trusted: NoteVerifier =
                verifier_key.parse().map_err(|error| anyhow::anyhow!("verifier key: {error:?}"))?;
            let report = export.verify(&[trusted]);
            Ok(Verification {
                accepted: report.is_valid(),
                report: report.to_string(),
                by: "signed_audit::verify (ADK_RUST_BIN unset)",
            })
        }
    }
}
