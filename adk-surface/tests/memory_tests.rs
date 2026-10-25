//! The web home's "Agent Memory and Rules" (spec 3.3 §5.1, §10 K5, §12 phases 3 and 4): every
//! kernel route the page calls for what agents remember, what they did and what each spent, on
//! a real kernel served beside the engine the way the home's server serves them, with the JSON
//! fields the page reads (`web/src/kernel.ts`).

#![cfg(feature = "web")]

mod common;

use std::sync::{Arc, Mutex};

use adk_core::principal::SubjectRef;
use adk_kernel::control::{
    ControlPlaneConfig, FactOrigin, MemoryBook, RecordBook, RecordCheck, RecordLine,
    RememberedFact, TidyUp, TidyUpUndo,
};
use adk_kernel::{KernelError, PartitionId};
use adk_model::{ScriptedLlm, ScriptedResponse};
use chrono::Utc;
use common::*;
use serde_json::{Value, json};

/// A memory of three facts and one finished tidy-up that merged two of them, corrected as the
/// harness's book corrects it.
#[derive(Default)]
struct Book {
    facts: Mutex<Vec<RememberedFact>>,
    undone: Mutex<bool>,
}

impl Book {
    fn new() -> Self {
        let at = Utc::now();
        let facts = vec![
            RememberedFact::new(
                "f1",
                "You bill [CLIENT] at [RATE] an hour.",
                FactOrigin::YouSaidThis,
                at,
            ),
            RememberedFact::new("f2", "Your accountant is [NAME].", FactOrigin::Guess, at)
                .with_noticed_by("work")
                .with_area("work")
                .with_tidy_up("tick-1"),
        ];
        Self { facts: Mutex::new(facts), undone: Mutex::new(false) }
    }
}

fn missing(id: &str) -> KernelError {
    KernelError::NotFound { pid: id.into() }
}

#[async_trait::async_trait]
impl MemoryBook for Book {
    async fn facts(
        &self,
        user: &str,
        _: Option<&PartitionId>,
    ) -> adk_kernel::Result<Vec<RememberedFact>> {
        assert_eq!(user, OWNER, "the book is read for the caller");
        Ok(self.facts.lock().unwrap().clone())
    }

    async fn confirm(
        &self,
        _: &str,
        id: &str,
        _: &SubjectRef,
    ) -> adk_kernel::Result<RememberedFact> {
        let mut facts = self.facts.lock().unwrap();
        let fact = facts.iter_mut().find(|fact| fact.id == id).ok_or_else(|| missing(id))?;
        fact.origin = FactOrigin::YouConfirmedThis;
        Ok(fact.clone())
    }

    async fn fix(
        &self,
        _: &str,
        id: &str,
        text: &str,
        _: &SubjectRef,
    ) -> adk_kernel::Result<RememberedFact> {
        let mut facts = self.facts.lock().unwrap();
        let fact = facts.iter_mut().find(|fact| fact.id == id).ok_or_else(|| missing(id))?;
        *fact = RememberedFact::new("f9", text, FactOrigin::YouConfirmedThis, Utc::now());
        Ok(fact.clone())
    }

    async fn forget(&self, _: &str, id: &str, _: &SubjectRef) -> adk_kernel::Result<()> {
        let mut facts = self.facts.lock().unwrap();
        let before = facts.len();
        facts.retain(|fact| fact.id != id);
        if facts.len() == before { Err(missing(id)) } else { Ok(()) }
    }

    async fn tidy_ups(&self, _: &str) -> adk_kernel::Result<Vec<TidyUp>> {
        let at = Utc::now() - chrono::Duration::hours(5);
        let mut tick = TidyUp::new("tick-1", "work", at).with_counts(1, 2, 0, 0).finished(at);
        if *self.undone.lock().unwrap() {
            tick = tick.undone(Utc::now());
        }
        Ok(vec![tick])
    }

    async fn undo_tidy_up(
        &self,
        _: &str,
        id: &str,
        _: &SubjectRef,
    ) -> adk_kernel::Result<TidyUpUndo> {
        *self.undone.lock().unwrap() = true;
        Ok(TidyUpUndo::new(id, vec!["f2a".into(), "f2b".into()], vec!["f2".into()], vec![]))
    }
}

/// A record of two lines that checks out, as `adk-rust up`'s signed record book answers.
struct Record;

#[async_trait::async_trait]
impl RecordBook for Record {
    async fn recent(&self, limit: usize) -> adk_kernel::Result<Vec<RecordLine>> {
        let lines = vec![
            RecordLine::new(2, Utc::now(), "surface.decide")
                .with_subject(OWNER)
                .with_tool("send_email")
                .with_decision("approved")
                .with_outcome("decided")
                .with_how("chat", Some("telegram"))
                .with_target("apr_1"),
            RecordLine::new(1, Utc::now(), "write_file")
                .with_agent("urn:adk:agent:home/finance")
                .with_tool("write_file")
                .with_outcome("succeeded"),
        ];
        Ok(lines.into_iter().take(limit).collect())
    }

    async fn check(&self) -> adk_kernel::Result<RecordCheck> {
        Ok(RecordCheck::new(true, 2, Utc::now()).with_report(json!({ "records": 2 })))
    }
}

async fn send(router: &axum::Router, method: &str, path: &str, body: Option<Value>) -> Value {
    let (status, value) = call(router, method, &format!("/api{path}"), body).await;
    assert!(status.is_success(), "{method} {path}: {status} {value}");
    value
}

/// What your agents remember and did, as the page reads and corrects it: each fact's text,
/// `origin` and `noticedBy`; each tidy-up's counts and `finishedAt`/`undoneAt`, which decide
/// whether Undo applies; the undo's `restored`/`forgotten`/`kept`; the record's lines and its
/// check's `valid`/`entries`/`checkedAt` (spec 3.3 §10 K5).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_home_reads_and_corrects_the_memory_and_checks_the_record() {
    let control = ControlPlaneConfig::default()
        .without_socket()
        .with_memory(Arc::new(Book::new()))
        .with_record(Arc::new(Record));
    let s = scenario_with(
        Arc::new(ScriptedLlm::new("idle", Vec::<ScriptedResponse>::new())),
        control,
        true,
    )
    .await;
    let router = api(&s.kernel, &s.engine);

    let memory = send(&router, "GET", "/kernel/memory", None).await;
    let guess = &memory["facts"][1];
    assert_eq!(
        (guess["id"].clone(), guess["origin"].clone(), guess["noticedBy"].clone()),
        (json!("f2"), json!("guess"), json!("work")),
        "{memory}"
    );
    assert!(guess["text"].is_string() && guess["learnedAt"].is_string(), "{memory}");
    let tick = &memory["tidyUps"][0];
    assert_eq!(
        [
            &tick["id"],
            &tick["area"],
            &tick["merged"],
            &tick["superseded"],
            &tick["forgotten"],
            &tick["distilled"]
        ],
        [&json!("tick-1"), &json!("work"), &json!(1), &json!(2), &json!(0), &json!(0)],
        "{memory}"
    );
    assert!(tick["finishedAt"].is_string() && tick.get("undoneAt").is_none(), "{memory}");

    let confirmed = send(&router, "POST", "/kernel/memory/facts/f2/confirm", Some(json!({}))).await;
    assert_eq!(confirmed["fact"]["origin"], "youConfirmedThis");
    let fixed =
        send(&router, "POST", "/kernel/memory/facts/f1/fix", Some(json!({ "text": "B" }))).await;
    assert_eq!(
        (fixed["fact"]["text"].clone(), fixed["fact"]["origin"].clone()),
        (json!("B"), json!("youConfirmedThis"))
    );
    send(&router, "POST", "/kernel/memory/facts/f9/forget", Some(json!({}))).await;
    let memory = send(&router, "GET", "/kernel/memory", None).await;
    assert_eq!(memory["facts"].as_array().unwrap().len(), 1, "a forgotten fact is gone: {memory}");

    let undone =
        send(&router, "POST", "/kernel/memory/undo", Some(json!({ "tidyUp": "tick-1" }))).await;
    assert_eq!(
        undone,
        json!({ "id": "tick-1", "restored": ["f2a", "f2b"], "forgotten": ["f2"], "kept": [] })
    );
    let memory = send(&router, "GET", "/kernel/memory", None).await;
    assert!(memory["tidyUps"][0]["undoneAt"].is_string(), "Undo applies once: {memory}");

    let record = send(&router, "GET", "/kernel/record?limit=200", None).await;
    let answer = &record["lines"][0];
    assert_eq!(
        [
            &answer["action"],
            &answer["tool"],
            &answer["decision"],
            &answer["outcome"],
            &answer["how"],
            &answer["channel"]
        ],
        [
            &json!("surface.decide"),
            &json!("send_email"),
            &json!("approved"),
            &json!("decided"),
            &json!("chat"),
            &json!("telegram")
        ],
        "{record}"
    );
    assert_eq!(record["lines"][1]["agent"], "urn:adk:agent:home/finance");
    let check = send(&router, "GET", "/kernel/record/check", None).await;
    assert_eq!(
        (check["valid"].clone(), check["entries"].clone()),
        (json!(true), json!(2)),
        "{check}"
    );
    assert!(check["checkedAt"].is_string(), "{check}");
}

/// A kernel set up without long-term memory or a signed record says so with `kernel.config`,
/// which the page reads as "no memory here" and falls back to the activity ledger, never as a
/// failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_kernel_without_memory_or_record_says_so() {
    let s = scenario(Arc::new(ScriptedLlm::new("idle", Vec::<ScriptedResponse>::new()))).await;
    let router = api(&s.kernel, &s.engine);
    for path in ["/api/kernel/memory", "/api/kernel/record/check", "/api/kernel/record?limit=200"] {
        let (status, body) = call(&router, "GET", path, None).await;
        assert_eq!(status.as_u16(), 400, "{path}: {body}");
        assert_eq!(body["error"]["code"], "kernel.config", "{path}: {body}");
    }
}

/// Each agent's spend today and its area, which the roster sums per area, come with every
/// process the kernel lists (`view.spendToday`, `meta.partition`), so an area's agents add up
/// to the area's "spent today" (spec 3.3 §12 phase 4).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_process_carries_its_spend_today_and_its_area() {
    let model = Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("Hello.")]));
    let s = scenario(model).await;
    let router = api(&s.kernel, &s.engine);
    s.engine
        .execute(&web(), adk_surface::Command::Send { session_id: "s1".into(), text: "Hi".into() })
        .await
        .unwrap();
    let processes = eventually("the turn's process", std::time::Duration::from_secs(20), || {
        let router = router.clone();
        async move {
            let page = send(&router, "GET", "/kernel/processes", None).await;
            (!page["processes"].as_array()?.is_empty()).then_some(page)
        }
    })
    .await;
    let process = &processes["processes"][0];
    assert_eq!(process["meta"]["partition"], "default", "{processes}");
    assert!(process["view"]["spendToday"].is_u64(), "{processes}");
    assert!(process["run"]["target"].is_string(), "{processes}");
    let settings = send(&router, "GET", "/kernel/settings", None).await;
    assert!(settings["areas"][0]["spentToday"].is_u64(), "{settings}");
}
