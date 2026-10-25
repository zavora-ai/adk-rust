//! The web home's Today, its morning briefing and its settings (spec 3.3 §5.1, §10 K1, K3,
//! K4, §12 phase 2): every kernel route the page calls, on a real kernel with a `/proc`
//! ledger served beside the engine the way the home's server serves them, called through
//! `ControlClient`'s typed methods, with the fields the page reads; and the engine's
//! briefing over the same ledger, in plain words.

#![cfg(feature = "web")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use adk_core::{RequestContext, RunState};
use adk_kernel::channels::{
    ChannelGateway, ChannelNotice, InMemoryChannel, InMemoryChannelBindingStore,
};
use adk_kernel::control::api::{AwayKind, AwayQuery};
use adk_kernel::control::{ControlClient, ControlPlaneConfig};
use adk_kernel::pairing::{InMemoryDeviceStore, Pairing};
use adk_kernel::proc::{Activity, InMemoryLedgerStore, Ledger, LedgerKey, LedgerKind, LedgerSink};
use adk_kernel::{PartitionMode, QuietHours};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_surface::{Answer, Command, CommandOutcome};
use common::*;
use serde_json::{Value, json};

const WITHIN: Duration = Duration::from_secs(20);

fn ledger() -> Ledger {
    Ledger::new(Arc::new(InMemoryLedgerStore::new()), LedgerKey::from_bytes([7; 32]))
}

fn control() -> ControlPlaneConfig {
    ControlPlaneConfig::default().without_socket()
}

/// `router` on a loopback port for as long as the guard lives.
struct Served {
    base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(router: axum::Router) -> Served {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Served { base, task }
}

/// The JSON the page reads from `path`, as the browser gets it.
async fn page(base: &str, path: &str) -> Value {
    reqwest::get(format!("{base}/api{path}")).await.unwrap().json().await.unwrap()
}

/// A run with one decision, allowed once from the home (spec 3.3 §12 phase 2; coordinator's
/// finding on `main`): "While you were away" is a short list in plain words — the decision
/// and the finished work, each agent by its name — never the kernel's housekeeping, raw
/// agent ids or process state changes; the engine's briefing shows Done from the ledger's
/// process rows; and the kernel's page has the same lines.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_decision_makes_a_short_plain_while_you_were_away() {
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "invoice.txt", "content": "INV-1" }),
            ),
            ScriptedResponse::text("The invoice is written."),
        ],
    ));
    let s = scenario_with_ledger(model, control(), ledger()).await;
    let since = chrono::Utc::now() - chrono::Duration::minutes(5);

    let sent = s
        .engine
        .execute(
            &web(),
            Command::Send { session_id: "s1".into(), text: "Write the invoice".into() },
        )
        .await
        .unwrap();
    assert_eq!(sent, CommandOutcome::Sent { session_id: "s1".into() });
    let card = eventually("the held write", WITHIN, || async {
        s.engine.decisions(&web()).await.unwrap().into_iter().next()
    })
    .await;
    let outcome = s
        .engine
        .execute(
            &web(),
            Command::Decide {
                decision_id: card.id.clone(),
                version: card.version,
                answer: Answer::AllowOnce,
                edited_args: None,
                reason: None,
                expanded: true,
            },
        )
        .await
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::Answered { .. }), "{outcome:?}");
    eventually("the turn to finish", WITHIN, || async {
        let processes = s.kernel.processes().ps(Default::default()).await.unwrap();
        processes.iter().any(|process| process.run.state == RunState::Completed).then_some(())
    })
    .await;

    let briefing = eventually("the finished work in the briefing", WITHIN, || async {
        let briefing = s.engine.briefing(&web(), since).await.unwrap();
        briefing.lines.iter().any(|line| line.text == "Finance finished.").then_some(briefing)
    })
    .await;
    let lines: Vec<(Option<&str>, &str)> =
        briefing.lines.iter().map(|line| (line.pill_text.as_deref(), line.text.as_str())).collect();
    assert!(lines.contains(&(Some("Done"), "Finance finished.")), "{lines:?}");
    assert!(lines.len() <= 3, "a short list: {lines:?}");
    for (_, text) in &lines {
        for noise in
            ["urn:", "Urn:", "Kernel", "changed what it is doing", "woke up", "spent money"]
        {
            assert!(!text.contains(noise), "{text:?} is housekeeping or a raw id");
        }
    }

    // The kernel's page, which Today and the morning briefing read, has the same lines.
    let served = serve(api(&s.kernel, &s.engine)).await;
    let client = ControlClient::http(&served.base);
    let away = client.away(&AwayQuery::default()).await.unwrap();
    let kernel_seqs: Vec<u64> = away.lines.iter().map(|line| line.seq).collect();
    let engine_seqs: Vec<u64> = briefing.lines.iter().map(|line| line.seq).collect();
    assert_eq!(kernel_seqs, engine_seqs, "one source: the same events on every surface");
    assert_eq!(away.counts.done, 1, "{away:?}");
}

/// Today reads `GET /kernel/away`, opens each line's record at `GET /kernel/away/lines/{seq}`,
/// and moves on only when the person marks it seen with `POST /kernel/away/seen` (spec 3.3
/// §5.1, §10 K4): reading it again, as opening the page does, shows the same lines.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_marking_it_seen_moves_while_you_were_away_on() {
    let s = scenario_with_ledger(
        Arc::new(ScriptedLlm::new("idle", Vec::<ScriptedResponse>::new())),
        control(),
        ledger(),
    )
    .await;
    let ledger = s.kernel.ledger().cloned().unwrap();
    let research = "urn:adk:agent:home/research";
    ledger.record(Activity::new(LedgerKind::Wake, "kernel").with_effect("woke")).await.unwrap();
    ledger
        .record(Activity::new(LedgerKind::Process, research).with_effect("completed"))
        .await
        .unwrap();
    ledger
        .record(Activity::new(LedgerKind::Process, "household").with_effect("failed"))
        .await
        .unwrap();
    ledger
        .record(Activity::new(LedgerKind::Spend, research).with_count("micro_usd", 620_000))
        .await
        .unwrap();
    ledger
        .record(Activity::new(LedgerKind::Proposal, "kernel").with_effect("proposed"))
        .await
        .unwrap();
    let served = serve(api(&s.kernel, &s.engine)).await;
    let client = ControlClient::http(&served.base);
    let home = AwayQuery { viewer: Some("home".into()), ..AwayQuery::default() };

    // The fields the page reads (`adk-surface/web/src/kernel.ts` `AwayPage`).
    let raw = page(&served.base, "/kernel/away?viewer=home").await;
    for field in [
        "viewer",
        "since",
        "through",
        "more",
        "counts",
        "spentMicroUsd",
        "quietTime",
        "waiting",
        "lines",
    ] {
        assert!(raw.get(field).is_some(), "the page reads `{field}`: {raw}");
    }
    assert_eq!(raw["quietTime"]["respected"], true);
    assert_eq!(raw["spentMicroUsd"], 620_000);
    for field in ["seq", "at", "partition", "agent", "kind", "record"] {
        assert!(raw["lines"][0].get(field).is_some(), "a line carries `{field}`: {raw}");
    }

    let first = client.away(&home).await.unwrap();
    let kinds: Vec<AwayKind> = first.lines.iter().map(|line| line.kind).collect();
    assert_eq!(kinds, [AwayKind::Done, AwayKind::Failed, AwayKind::Suggestion]);
    // Opening the page again, any number of times, moves nothing: a viewer with no place yet
    // reads the last day each time.
    let again = client.away(&home).await.unwrap();
    assert_eq!((again.lines, again.through), (first.lines.clone(), first.through));

    // Every line links to its record.
    for line in &first.lines {
        assert_eq!(line.record, format!("/api/kernel/away/lines/{}", line.seq));
        let record = client.away_line(line.seq).await.unwrap();
        assert_eq!((record.entry.seq, record.line.as_ref()), (line.seq, Some(line)));
        let raw = page(&served.base, &format!("/kernel/away/lines/{}", line.seq)).await;
        assert_eq!(raw["entry"]["seq"], line.seq, "the page reads the record's entry: {raw}");
    }

    // Marked seen through the page's last row: the next read starts after it.
    let through = first.through.unwrap();
    let cursor = client.mark_away_seen(Some("home"), through).await.unwrap();
    assert_eq!(cursor.seq, through);
    let after = client.away(&home).await.unwrap();
    assert!(after.lines.is_empty(), "{after:?}");
    assert!(after.since > first.since);
    // Another surface keeps its own place.
    let terminal = client
        .away(&AwayQuery { viewer: Some("terminal".into()), ..AwayQuery::default() })
        .await
        .unwrap();
    assert_eq!(terminal.lines.len(), 3);
}

/// "What your agents can do" reads every area's quiet time, daily spending limit with what was
/// spent today, and mode from `GET /kernel/settings`, and changes each with its `POST` (spec 3.3
/// §10 K3); the answer is the area after the change, which the page shows beside the value it
/// read just before. A caller without the `operator` scope changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_home_reads_and_changes_an_area_s_settings() {
    let s = scenario_with_ledger(
        Arc::new(ScriptedLlm::new("idle", Vec::<ScriptedResponse>::new())),
        control().with_start_mode("default", PartitionMode::Ask),
        ledger(),
    )
    .await;
    let served = serve(api(&s.kernel, &s.engine)).await;
    let client = ControlClient::http(&served.base);

    let raw = page(&served.base, "/kernel/settings").await;
    let area = &raw["areas"][0];
    assert_eq!(area["partition"], "default", "{raw}");
    assert_eq!(area["mode"], "ask");
    assert!(area.get("quietTime").is_some() && area.get("spentToday").is_some(), "{raw}");

    let window: QuietHours = "23:00-06:00".parse().unwrap();
    let before = client.area_settings("default").await.unwrap();
    let after = client.set_quiet_time("default", Some(window)).await.unwrap();
    assert_ne!(before.quiet_time, after.quiet_time);
    assert_eq!(after.quiet_time, [window]);
    let after = client.set_spending_limit("default", Some(8_000_000)).await.unwrap();
    assert_eq!(after.daily_spending_limit, Some(8_000_000));
    let after = client.set_area_mode("default", PartitionMode::Auto).await.unwrap();
    assert_eq!(after.mode, Some(PartitionMode::Auto));
    let raw = page(&served.base, "/kernel/settings").await;
    let changed = &raw["areas"][0]["changed"];
    assert_eq!(changed["quietTime"]["value"], json!({ "start": "23:00", "end": "06:00" }), "{raw}");
    assert_eq!(changed["dailySpendingLimit"]["value"], 8_000_000);
    assert!(changed["mode"]["at"].is_string(), "the page says when it changed: {raw}");

    // Someone the server admitted without the operator scope reads, and changes nothing.
    let viewer = axum::Router::new()
        .nest("/api", adk_kernel::control::router(s.kernel.clone()))
        .layer(axum::Extension(Some(RequestContext {
            user_id: OWNER.into(),
            scopes: vec!["owner".into()],
            metadata: Default::default(),
        })));
    let viewer = serve(viewer).await;
    let refused = ControlClient::http(&viewer.base).set_quiet_time("default", None).await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(client.area_settings("default").await.unwrap().quiet_time, [window]);
}

/// "Where you can say yes from" pairs the chat account a stranger's message was sent a code
/// for, with the same claim as `adk-rust pair accept <code>` (spec 3.3 §10 K1): the account
/// waits until then, and is listed as paired after.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_home_pairs_a_chat_account_with_its_code() {
    let bindings = Arc::new(InMemoryChannelBindingStore::new());
    let pairing =
        Pairing::new(Arc::new(InMemoryDeviceStore::new())).with_channel_bindings(bindings.clone());
    let s = scenario_with_ledger(
        Arc::new(ScriptedLlm::new("idle", Vec::<ScriptedResponse>::new())),
        control().with_pairing(pairing),
        ledger(),
    )
    .await;
    let chat = Arc::new(InMemoryChannel::new("telegram"));
    let gateway = ChannelGateway::builder(Arc::new(s.kernel.clone()), bindings)
        .sink(chat.clone())
        .target(APP)
        .build();
    gateway.receive(chat.text("stranger", "t1", "hello")).await.unwrap();
    let code = chat
        .notices("t1")
        .into_iter()
        .find_map(|notice| match notice {
            ChannelNotice::PairingCode { code, .. } => Some(code),
            _ => None,
        })
        .expect("the stranger was sent a code");

    let served = serve(api(&s.kernel, &s.engine)).await;
    let client = ControlClient::http(&served.base);
    let raw = page(&served.base, "/kernel/channels").await;
    assert_eq!(raw["waiting"][0]["channel"], "telegram", "the page lists who waits: {raw}");
    assert_eq!(raw["accounts"], json!([]));

    let account = client.accept_channel(&code).await.unwrap();
    assert_eq!(account.channel, "telegram");
    let raw = page(&served.base, "/kernel/channels").await;
    assert_eq!(raw["accounts"][0]["state"], "active", "{raw}");
    assert_eq!(raw["waiting"], json!([]));
    // A code works once.
    assert!(client.accept_channel(&code).await.is_err());
}
