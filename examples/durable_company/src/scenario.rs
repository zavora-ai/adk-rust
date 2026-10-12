//! The operator's side of the day: register the schedule, start and kill replicas, decide
//! the approval, freeze the company, and verify the audit log. Every step checks what the
//! stores hold and prints it as evidence.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use adk_auth::signed_audit::{AuditExport, AuditSigner, Ed25519Signer, NoteVerifier};
use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalDecision, ApprovalService, ApprovalState, InboxQuery, MissedFirePolicy, NewRun,
    RunCheckpointRef, RunFilter, RunRecord, RunState, Schedule, ScheduleSpec, SpendKey,
    SpendPeriod,
};
use anyhow::{Context, bail, ensure};
use serde_json::{Value, json};

use crate::company::{
    AGENT_ID, APP, Bank, CONTROLLER, DAILY_CAP_USD, DUE_TODAY, Provider, QUEUE, QUORUM, RUSH,
    TARGET,
};
use crate::stores::{Backend, Stores};
use crate::worker::{LEASE, Replica, approval_service, audit_log, kill_switch};

const DAILY_SCHEDULE: &str = "ap-daily";
const MONTH_END_SCHEDULE: &str = "month-end-close";
const CONSOLE_LOG: &str = "audit/console";
const CFO: &str = "cfo@acme.example";
const APPROVERS: [&str; 2] = ["dana@acme.example", "eli@acme.example"];

/// A replica as a separate OS process, or as tasks in this one for the in-memory backend.
enum Worker {
    Process { id: String, child: Child },
    InProcess { id: String, replica: Replica },
}

/// What the scenario needs to start replicas.
struct Day {
    backend: Backend,
    provider: Provider,
    data_dir: PathBuf,
    stores: Stores,
    bank: Bank,
    evidence: Vec<(String, String)>,
    started: Instant,
}

impl Day {
    fn record(&mut self, step: &str, evidence: impl Into<String>) {
        let evidence = evidence.into();
        println!("[{:>6.1}s] {step}: {evidence}", self.started.elapsed().as_secs_f64());
        self.evidence.push((step.to_string(), evidence));
    }

    async fn start_worker(&self, id: &str, crash_marker: Option<&Path>) -> anyhow::Result<Worker> {
        if let Backend::Memory = self.backend {
            let replica = Replica::start(
                &self.stores,
                self.provider,
                &self.data_dir,
                id,
                crash_marker.map(Path::to_path_buf),
            )
            .await?;
            return Ok(Worker::InProcess { id: id.to_string(), replica });
        }
        let log = std::fs::File::create(self.data_dir.join(format!("{id}.log")))?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("worker")
            .args(self.backend.worker_args())
            .args(["--provider", self.provider.label(), "--worker-id", id])
            .arg("--data-dir")
            .arg(&self.data_dir)
            .stdout(Stdio::null())
            .stderr(log);
        if let Some(marker) = crash_marker {
            command.arg("--crash-marker").arg(marker);
        }
        let child = command.spawn().context("starting a worker process")?;
        Ok(Worker::Process { id: id.to_string(), child })
    }

    /// Kills `worker` without letting it write anything: SIGKILL, or an abort in memory.
    fn kill(worker: Worker) -> anyhow::Result<String> {
        match worker {
            Worker::Process { id, mut child } => {
                let pid = child.id();
                child.kill()?;
                child.wait()?;
                Ok(format!("{id} (pid {pid}) killed with SIGKILL"))
            }
            Worker::InProcess { id, replica } => {
                replica.abort();
                Ok(format!("{id} aborted in process"))
            }
        }
    }

    /// Asks `worker` to stop and waits for it.
    async fn stop(&self, worker: Worker) -> anyhow::Result<()> {
        match worker {
            Worker::Process { id, mut child } => {
                std::fs::write(self.data_dir.join(format!("stop-{id}")), "")?;
                let deadline = Instant::now() + Duration::from_secs(60);
                loop {
                    if let Some(status) = child.try_wait()? {
                        ensure!(status.success(), "{id} exited with {status}");
                        return Ok(());
                    }
                    if Instant::now() > deadline {
                        child.kill().ok();
                        bail!("{id} did not stop");
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            Worker::InProcess { replica, .. } => replica.stop().await,
        }
    }

    fn ensure_alive(&self, worker: &mut Worker) -> anyhow::Result<()> {
        if let Worker::Process { id, child } = worker
            && let Some(status) = child.try_wait()?
        {
            let log = std::fs::read_to_string(self.data_dir.join(format!("{id}.log")))
                .unwrap_or_default();
            let tail: Vec<&str> = log.lines().rev().take(15).collect();
            bail!(
                "{id} exited early with {status}; log tail:\n{}",
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            );
        }
        Ok(())
    }

    async fn run(&self, run_id: &str) -> anyhow::Result<RunRecord> {
        self.stores.runs.get(run_id).await?.with_context(|| format!("run {run_id} vanished"))
    }

    /// Polls `run_id` until `done` holds, failing after `timeout`.
    async fn wait_for(
        &self,
        worker: &mut Worker,
        run_id: &str,
        what: &str,
        timeout: Duration,
        done: impl Fn(&RunRecord) -> bool,
    ) -> anyhow::Result<RunRecord> {
        let deadline = Instant::now() + timeout;
        loop {
            let run = self.run(run_id).await?;
            if done(&run) {
                return Ok(run);
            }
            ensure!(
                !run.state.is_terminal(),
                "run {run_id} ended {:?} while waiting until {what}: {}",
                run.state,
                run.error.as_deref().unwrap_or("")
            );
            self.ensure_alive(worker)?;
            ensure!(Instant::now() < deadline, "timed out waiting until {what}: {:?}", run.state);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn result_text(run: &RunRecord) -> String {
    let text = run.result.as_ref().and_then(|result| result["text"].as_str()).unwrap_or_default();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn payments_of(bank: &Bank, invoice_id: &str) -> usize {
    bank.payments().iter().filter(|payment| payment["invoiceId"] == invoice_id).count()
}

/// Runs the whole day and returns the evidence, step by step.
pub async fn run(
    backend: Backend,
    provider: Provider,
    data_dir: PathBuf,
) -> anyhow::Result<Vec<(String, String)>> {
    let stores = Stores::open(&backend).await?;
    let bank = Bank::new(&data_dir);
    let mut day = Day {
        backend: backend.clone(),
        provider,
        data_dir: data_dir.clone(),
        stores: stores.clone(),
        bank,
        evidence: Vec::new(),
        started: Instant::now(),
    };
    day.record(
        "setup",
        format!(
            "backend {}, provider {} ({}), data {}",
            backend.label(),
            provider.label(),
            provider.model_id(),
            data_dir.display()
        ),
    );

    // 0. The audit signing key, shared by every replica (a secret in a real deployment).
    let signer = Ed25519Signer::generate("audit.acme.example")?;
    let key_path = data_dir.join("audit.key");
    std::fs::write(&key_path, signer.to_note_key().as_str())?;
    let verifier_key = signer.verifier_key();
    std::fs::write(data_dir.join("audit.vkey"), format!("{verifier_key}\n"))?;
    let console = audit_log(&stores, &key_path, CONSOLE_LOG).await?;
    let trail = AuditTrail::new(console.clone());

    // 1. The daily accounts-payable schedule; its first occurrence is due now.
    let template =
        NewRun::new(QUEUE, APP, CONTROLLER, TARGET, json!("Pay the invoices due today."));
    let now = chrono::Utc::now();
    stores
        .schedules
        .upsert(
            Schedule::new(
                DAILY_SCHEDULE,
                ScheduleSpec::Cron { expr: "0 9 * * *".into(), timezone: "UTC".into() },
                template,
                MissedFirePolicy::FireOnce,
            )
            .with_next_fire_at(now),
        )
        .await?;
    day.record(
        "schedule",
        format!("'{DAILY_SCHEDULE}' cron 0 9 * * * UTC registered, first occurrence due {now}"),
    );

    // 2. Replica A's scheduler fires the occurrence and its worker claims the run; A is
    //    killed inside the invoice lookup.
    let crash_marker = data_dir.join("crash-reached");
    let mut a = day.start_worker("worker-a", Some(&crash_marker)).await?;
    let deadline = Instant::now() + Duration::from_secs(120);
    let fired = loop {
        let runs = stores.runs.list(RunFilter::default().queue(QUEUE)).await?;
        if let Some(run) = runs.into_iter().next() {
            break run;
        }
        day.ensure_alive(&mut a)?;
        ensure!(Instant::now() < deadline, "the schedule never fired");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let schedule = stores.schedules.get(DAILY_SCHEDULE).await?.context("schedule vanished")?;
    day.record(
        "cron fired",
        format!(
            "run {} enqueued with key {}; schedule advanced to {}",
            fired.run_id,
            fired.idempotency_key.as_deref().unwrap_or("-"),
            schedule.next_fire_at.map(|at| at.to_rfc3339()).unwrap_or_default()
        ),
    );
    let run_id = fired.run_id.clone();
    let deadline = Instant::now() + Duration::from_secs(180);
    while !crash_marker.exists() {
        day.ensure_alive(&mut a)?;
        let run = day.run(&run_id).await?;
        ensure!(
            !run.state.is_terminal(),
            "run ended before the crash point: {:?} {:?}",
            run.state,
            run.error
        );
        ensure!(Instant::now() < deadline, "worker-a never reached list_due_invoices");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let killed = Day::kill(a)?;
    let stopped = day.run(&run_id).await?;
    ensure!(
        stopped.state == RunState::Running
            && matches!(stopped.checkpoint, Some(RunCheckpointRef::Session { .. })),
        "after the kill the run is {:?} with checkpoint {:?}",
        stopped.state,
        stopped.checkpoint
    );
    day.record(
        "crash",
        format!(
            "{killed} inside list_due_invoices; run {} still Running at attempt {} owned by {}, checkpoint Session",
            run_id,
            stopped.attempt,
            stopped.lease_owner.as_deref().unwrap_or("-")
        ),
    );

    // 3. Replica B recovers the run once A's lease expires, pays what policy allows, and
    //    parks on the payment that needs approval.
    let killed_at = Instant::now();
    let mut b = day.start_worker("worker-b", None).await?;
    let recovered = day
        .wait_for(&mut b, &run_id, "worker-b claims it", Duration::from_secs(60), |run| {
            run.attempt >= 2 || run.lease_owner.as_deref() == Some("worker-b")
        })
        .await?;
    day.record(
        "resume",
        format!(
            "worker-b claimed run {run_id} {:.1}s after the kill (lease {}s), attempt {}",
            killed_at.elapsed().as_secs_f64(),
            LEASE.as_secs(),
            recovered.attempt
        ),
    );
    let parked = day
        .wait_for(
            &mut b,
            &run_id,
            "the run parks on an approval",
            Duration::from_secs(240),
            |run| run.state == RunState::Waiting,
        )
        .await?;
    let Some(RunCheckpointRef::AwaitingApproval { fingerprints, .. }) = &parked.checkpoint else {
        bail!("the run is waiting without an approval checkpoint: {:?}", parked.checkpoint);
    };
    let small = DUE_TODAY[0].id;
    let large = DUE_TODAY[1].id;
    ensure!(payments_of(&day.bank, large) == 0, "{large} was paid before any approval");
    day.record(
        "park",
        format!(
            "run {run_id} Waiting (attempt {}), {} held call(s); {small} ({} USD, under the threshold) paid {}x; {large} ({} USD) paid 0x",
            parked.attempt,
            fingerprints.len(),
            DUE_TODAY[0].amount_usd,
            payments_of(&day.bank, small),
            DUE_TODAY[1].amount_usd
        ),
    );

    // 4. Two finance approvers decide; the requester cannot approve their own payment.
    let service = approval_service(&stores).with_audit(trail.clone());
    let inbox = service.inbox(InboxQuery::for_group("finance")).await?;
    let request = inbox
        .items
        .iter()
        .find(|request| request.run_id.as_deref() == Some(run_id.as_str()))
        .cloned()
        .context("no approval request carries the run id")?;
    ensure!(request.tool_name == "pay_invoice", "the request is for {}", request.tool_name);
    let finance =
        |name: &str| ApprovalDecision::approve(SubjectRef::human(name)).with_groups(["finance"]);
    let own = service.decide(&request.id, request.version, finance(CONTROLLER)).await;
    let refused = match &own {
        Err(error) if error.code == "approval.self_approval" => error.code,
        other => bail!("self-approval was not refused: {other:?}"),
    };
    let half = service.decide(&request.id, request.version, finance(APPROVERS[0])).await?;
    ensure!(half.state == ApprovalState::Pending, "one approval resolved the request");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let still = day.run(&run_id).await?;
    ensure!(still.state == RunState::Waiting, "one approval woke the run: {:?}", still.state);
    ensure!(payments_of(&day.bank, large) == 0, "one approval paid {large}");
    day.record(
        "approval 1/2",
        format!(
            "request {} for pay_invoice {} (quorum {QUORUM}, requester {}): {CONTROLLER} refused with {refused}; {} approved -> Pending, run still Waiting, 0 payments",
            request.id,
            request.args_redacted,
            request.requester.as_ref().map(|subject| subject.id.as_str()).unwrap_or("-"),
            APPROVERS[0]
        ),
    );
    let resolved = service.decide(&half.id, half.version, finance(APPROVERS[1])).await?;
    ensure!(
        resolved.state == ApprovalState::Approved,
        "two approvals left it {:?}",
        resolved.state
    );
    let woken_at = Instant::now();
    let finished = day
        .wait_for(&mut b, &run_id, "the run completes", Duration::from_secs(240), |run| {
            run.state.is_terminal()
        })
        .await?;
    ensure!(
        finished.state == RunState::Completed,
        "run 1 ended {:?}: {:?}",
        finished.state,
        finished.error
    );
    day.record(
        "approval 2/2",
        format!(
            "{} approved -> Approved; run woke and completed {:.1}s later at attempt {}; payments: {small} {}x, {large} {}x",
            APPROVERS[1],
            woken_at.elapsed().as_secs_f64(),
            finished.attempt,
            payments_of(&day.bank, small),
            payments_of(&day.bank, large)
        ),
    );
    ensure!(
        payments_of(&day.bank, small) == 1 && payments_of(&day.bank, large) == 1,
        "each due invoice must be paid exactly once: {:?}",
        day.bank.payments()
    );
    day.record("run 1 answer", result_text(&finished));

    // 5. A rush payment the policy allows but the daily spend cap does not.
    let rush = stores
        .runs
        .enqueue(
            NewRun::new(
                QUEUE,
                APP,
                CONTROLLER,
                TARGET,
                json!(format!(
                    "Pay invoice {} from {} for {} USD now.",
                    RUSH.id, RUSH.vendor, RUSH.amount_usd
                )),
            )
            .with_idempotency_key(format!("rush:{}", RUSH.id)),
        )
        .await?
        .record()
        .cloned()
        .context("the rush run was rejected")?;
    let rushed = day
        .wait_for(&mut b, &rush.run_id, "the rush run finishes", Duration::from_secs(180), |run| {
            run.state.is_terminal()
        })
        .await?;
    let spent = stores.spend.spent(&SpendKey::org(APP), SpendPeriod::Day).await?;
    let paid_minor: i64 =
        day.bank.payments().iter().filter_map(|payment| payment["amountMinor"].as_i64()).sum();
    ensure!(payments_of(&day.bank, RUSH.id) == 0, "{} was paid past the cap", RUSH.id);
    ensure!(spent <= DAILY_CAP_USD * 1_000_000, "spend {spent} passed the cap");
    day.record(
        "spend cap",
        format!(
            "rush run {} {:?}: {} ({} USD) allowed by policy, refused by the {} USD/day cap before the bank, 0 payments; ledger {:.4} USD spent today ({} USD payments + {:.4} USD model)",
            rush.run_id,
            rushed.state,
            RUSH.id,
            RUSH.amount_usd,
            DAILY_CAP_USD,
            spent as f64 / 1e6,
            paid_minor / 100,
            (spent as f64 / 1e6) - (paid_minor as f64 / 100.0)
        ),
    );
    day.record("run 2 answer", result_text(&rushed));

    // 6. The CFO freezes the company: nothing more fires or runs on any replica.
    let control = kill_switch(&stores);
    control.attach_audit(trail.clone());
    control.refresh().await?;
    control
        .freeze_recorded(
            "month-end close: all payments paused",
            AuditActor::subject(SubjectRef::human(CFO)),
        )
        .await?;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let blocked = stores
        .runs
        .enqueue(NewRun::new(QUEUE, APP, CONTROLLER, TARGET, json!("Pay the invoices due today.")))
        .await?
        .record()
        .cloned()
        .context("the blocked run was rejected")?;
    stores
        .schedules
        .upsert(
            Schedule::new(
                MONTH_END_SCHEDULE,
                ScheduleSpec::At(chrono::Utc::now()),
                NewRun::new(QUEUE, APP, CONTROLLER, TARGET, json!("Close the month.")),
                MissedFirePolicy::FireOnce,
            )
            .with_next_fire_at(chrono::Utc::now()),
        )
        .await?;
    tokio::time::sleep(Duration::from_secs(6)).await;
    day.ensure_alive(&mut b)?;
    let waiting = day.run(&blocked.run_id).await?;
    let month_end = stores.schedules.get(MONTH_END_SCHEDULE).await?.context("schedule vanished")?;
    let queued = stores.runs.list(RunFilter::default().queue(QUEUE)).await?;
    ensure!(
        waiting.state == RunState::Queued && waiting.attempt == 0,
        "a run started while frozen: {waiting:?}"
    );
    ensure!(month_end.next_fire_at.is_some() && queued.len() == 3, "a schedule fired while frozen");
    let state = stores.governance.list().await?;
    day.record(
        "freeze",
        format!(
            "{CFO} froze the org ({}); 6s later run {} is still Queued at attempt 0, '{MONTH_END_SCHEDULE}' due since {} has not fired, queue holds {} runs",
            state.first().map(|state| format!("epoch {}, frozen {}", state.epoch, state.frozen)).unwrap_or_default(),
            blocked.run_id,
            month_end.next_fire_at.map(|at| at.to_rfc3339()).unwrap_or_default(),
            queued.len()
        ),
    );
    stores.runs.request_cancel(&blocked.run_id).await?;
    day.stop(b).await?;
    console.checkpoint().await?;

    // 7. Every log verifies with `adk-rust audit verify`; an edited record does not.
    let mut lines = Vec::new();
    for log_id in ["audit/worker-a", "audit/worker-b", CONSOLE_LOG] {
        let file = data_dir.join(format!("{}.jsonl", log_id.replace('/', "-")));
        let exported_by = export(&backend, &stores, log_id, &file).await?;
        let export_text = std::fs::read_to_string(&file)?;
        let parsed = AuditExport::from_jsonl(&export_text)
            .map_err(|(line, error)| anyhow::anyhow!("{log_id}:{line}: {error}"))?;
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        let mut actors: BTreeMap<String, usize> = BTreeMap::new();
        for entry in &parsed.entries {
            let record: Value = serde_json::from_str(entry.record.as_deref().unwrap_or("{}"))?;
            if log_id != CONSOLE_LOG {
                ensure!(
                    record["actor"]["agent"] == AGENT_ID,
                    "{log_id} record {} acts as {}",
                    entry.seq,
                    record["actor"]["agent"]
                );
            }
            let actor = [
                &record["actor"]["agent"],
                &record["actor"]["instance"],
                &record["actor"]["onBehalfOf"]["id"],
            ]
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
            *actors.entry(actor).or_default() += 1;
            let label = [&record["action"], &record["decision"], &record["outcome"]]
                .iter()
                .filter_map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            *kinds.entry(label).or_default() += 1;
        }
        let (accepted, report) = verify(&file, &verifier_key)?;
        ensure!(accepted, "{log_id} does not verify: {report}");
        let summary: Vec<String> =
            kinds.iter().map(|(label, count)| format!("{label} x{count}")).collect();
        let actors: Vec<String> =
            actors.iter().map(|(actor, count)| format!("{actor} x{count}")).collect();
        lines.push(format!(
            "{log_id}: {} records [{}] by [{}]; exported by {exported_by}; {report}",
            parsed.entries.len(),
            summary.join(", "),
            actors.join(", ")
        ));
        if log_id == "audit/worker-b" {
            let mut tampered = parsed.clone();
            if let Some(entry) = tampered.entries.iter_mut().find(|entry| {
                entry.record.as_deref().is_some_and(|record| record.contains("\"allowed\""))
            }) {
                entry.record = entry
                    .record
                    .take()
                    .map(|record| record.replacen("\"allowed\"", "\"denied\"", 1));
            }
            let edited = data_dir.join("audit-worker-b-edited.jsonl");
            std::fs::write(&edited, tampered.to_jsonl())?;
            let (accepted, report) = verify(&edited, &verifier_key)?;
            ensure!(!accepted, "an edited log still verifies");
            lines.push(format!("audit/worker-b with one decision edited: {report}"));
        }
    }
    for line in lines {
        day.record("audit", line);
    }
    Ok(day.evidence)
}

/// Exports `log_id` to `file` with `adk-rust audit export` when the CLI is available and
/// the store is SQL, and with the library otherwise.
async fn export(
    backend: &Backend,
    stores: &Stores,
    log_id: &str,
    file: &Path,
) -> anyhow::Result<&'static str> {
    if let (Ok(bin), Some(url)) = (std::env::var("ADK_RUST_BIN"), backend.audit_database_url()) {
        let output = Command::new(bin)
            .args(["audit", "export", "--database-url", &url, "--log-id", log_id, "--out"])
            .arg(file)
            .output()?;
        ensure!(
            output.status.success(),
            "adk-rust audit export failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok("adk-rust audit export");
    }
    std::fs::write(file, AuditExport::from_store(stores.audit.as_ref(), log_id).await?.to_jsonl())?;
    Ok("AuditExport::from_store")
}

/// Verifies `file` with `adk-rust audit verify` when `ADK_RUST_BIN` names the CLI, and
/// with the library otherwise.
fn verify(file: &Path, verifier_key: &str) -> anyhow::Result<(bool, String)> {
    match std::env::var("ADK_RUST_BIN") {
        Ok(bin) => {
            let output = Command::new(bin)
                .args(["audit", "verify"])
                .arg(file)
                .args(["--key", verifier_key])
                .output()?;
            let report = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            Ok((
                output.status.success(),
                format!(
                    "adk-rust audit verify (exit {}): {}",
                    output.status.code().unwrap_or(-1),
                    brief(&report)
                ),
            ))
        }
        Err(_) => {
            let trusted: NoteVerifier =
                verifier_key.parse().map_err(|error| anyhow::anyhow!("verifier key: {error:?}"))?;
            let export = AuditExport::from_jsonl(&std::fs::read_to_string(file)?)
                .map_err(|(line, error)| anyhow::anyhow!("{}:{line}: {error}", file.display()))?;
            let report = export.verify(&[trusted]);
            Ok((report.is_valid(), format!("library verify: {}", brief(&report.to_string()))))
        }
    }
}

/// The lines of a verify report that state the verdict: what is signed, and the first failure.
fn brief(report: &str) -> String {
    let mut lines: Vec<String> = report
        .lines()
        .filter(|line| {
            ["signed through:", "unsigned tail:", "result:"].iter().any(|key| line.starts_with(key))
        })
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    if let Some(failure) = report.lines().find(|line| line.trim_start().starts_with('[')) {
        lines.push(failure.trim().to_string());
    }
    lines.join("; ")
}
