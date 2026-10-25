//! "While you were away" is exact across restarts (spec 3.3 §10 K4, §12 phase 2 "its
//! counts and spending match the ledger exactly across the restart"): whatever rows are
//! written, whatever pages a viewer reads and marks seen, and wherever the kernel restarts
//! over its SQLite file, the lines a viewer marked seen are every line of the ledger once,
//! in order, and their spend is the ledger's.

#![cfg(all(feature = "proc", feature = "sqlite"))]

use std::sync::Arc;

use adk_core::InMemoryRunStore;
use adk_kernel::control::api::AwayQuery;
use adk_kernel::control::{ControlClient, ControlPlane, ControlPlaneConfig, SqliteControlStore};
use adk_kernel::proc::{Activity, Ledger, LedgerKey, LedgerKind, LedgerSink, SqliteLedgerStore};
use adk_kernel::{Kernel, KernelConfig};
use proptest::prelude::*;

/// What a test does next.
#[derive(Debug, Clone)]
enum Op {
    /// Writes one ledger row of kind `0..6`.
    Append(u8),
    /// Reads a page of at most `limit` rows, and marks it seen when `mark`.
    Read { limit: usize, mark: bool },
    /// Restarts the kernel over the same database file.
    Restart,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0u8..6).prop_map(Op::Append),
        3 => (1usize..6, any::<bool>()).prop_map(|(limit, mark)| Op::Read { limit, mark }),
        1 => Just(Op::Restart),
    ]
}

/// Row `kind`, whether it makes a line, and what it spends.
fn activity(kind: u8) -> (Activity, bool, u64) {
    match kind {
        0 => (Activity::new(LedgerKind::Process, "finance").with_effect("completed"), true, 0),
        1 => (Activity::new(LedgerKind::Wake, "kernel").with_effect("woke"), false, 0),
        2 => (Activity::new(LedgerKind::Proposal, "kernel").with_effect("proposed"), true, 0),
        3 => (
            Activity::new(LedgerKind::Spend, "finance").with_count("micro_usd", 1_250),
            false,
            1_250,
        ),
        4 => (Activity::new(LedgerKind::Observation, "kernel"), true, 0),
        _ => (Activity::new(LedgerKind::ToolCall, "finance").with_effect("read_only"), false, 0),
    }
}

/// A kernel over the SQLite file `url`, serving its control plane on loopback.
struct Running {
    plane: ControlPlane,
    client: ControlClient,
    ledger: Ledger,
    _kernel: Kernel,
}

impl Running {
    async fn start(url: &str) -> Self {
        let pool = sqlx::SqlitePool::connect(url).await.unwrap();
        let control = SqliteControlStore::new(pool.clone());
        control.migrate().await.unwrap();
        let rows = SqliteLedgerStore::new(pool);
        rows.migrate().await.unwrap();
        let config = KernelConfig::default().with_control(
            ControlPlaneConfig::default()
                .without_socket()
                .with_http("127.0.0.1:0".parse().unwrap())
                .with_control_store(Arc::new(control)),
        );
        let kernel = Kernel::builder()
            .config(config)
            .run_store(Arc::new(InMemoryRunStore::new()))
            .ledger(Ledger::new(Arc::new(rows), LedgerKey::from_bytes([5; 32])))
            .build()
            .unwrap();
        let plane = ControlPlane::start(&kernel).await.unwrap();
        let ledger = kernel.ledger().cloned().unwrap();
        let client = ControlClient::http(format!("http://{}", plane.http_addr().unwrap()));
        Self { plane, client, ledger, _kernel: kernel }
    }

    async fn stop(self) {
        self.plane.shutdown().await.unwrap();
    }
}

/// Reads a page of at most `limit` rows and, when `mark`, marks it seen; returns the
/// marked page's line sequence numbers and spend, and whether more rows wait.
async fn read(client: &ControlClient, limit: usize, mark: bool) -> (Vec<u64>, u64, bool) {
    let query = AwayQuery { limit: Some(limit), ..AwayQuery::default() };
    let page = client.away(&query).await.unwrap();
    let lines: Vec<u64> = page.lines.iter().map(|line| line.seq).collect();
    match (mark, page.through) {
        (true, Some(through)) => {
            let cursor = client.mark_away_seen(None, through).await.unwrap();
            assert_eq!(cursor.seq, through);
            (lines, page.spent_micro_usd, page.more)
        }
        _ => (Vec::new(), 0, page.more || page.through.is_some()),
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|cases| cases.parse().ok()).unwrap_or(100), ..ProptestConfig::default() })]

    /// Every line reaches the viewer once and in order, with the ledger's spend, across any
    /// number of restarts and any pages read, marked or not.
    #[test]
    fn away_delivers_every_line_once_across_restarts(ops in proptest::collection::vec(op(), 1..30)) {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().unwrap();
            let url = format!("sqlite://{}?mode=rwc", dir.path().join("harness.db").display());
            let mut running = Running::start(&url).await;
            let (mut seq, mut lines, mut spent) = (0u64, Vec::new(), 0u64);
            let (mut delivered, mut delivered_spend) = (Vec::new(), 0u64);
            for op in ops {
                match op {
                    Op::Append(kind) => {
                        let (activity, line, cost) = activity(kind);
                        running.ledger.record(activity).await.unwrap();
                        seq += 1;
                        if line {
                            lines.push(seq);
                        }
                        spent += cost;
                    }
                    Op::Read { limit, mark } => {
                        let (page, cost, _) = read(&running.client, limit, mark).await;
                        delivered.extend(page);
                        delivered_spend += cost;
                    }
                    Op::Restart => {
                        running.stop().await;
                        running = Running::start(&url).await;
                    }
                }
            }
            loop {
                let (page, cost, more) = read(&running.client, 50, true).await;
                let done = page.is_empty() && !more;
                delivered.extend(page);
                delivered_spend += cost;
                if done {
                    break;
                }
            }
            assert_eq!(delivered, lines, "every line once, in order");
            assert_eq!(delivered_spend, spent, "the spend is the ledger's");
            running.stop().await;
        });
    }
}
