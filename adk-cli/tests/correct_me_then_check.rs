//! Spec 3.3 §12 phase 3 "Correct me, then check" (§5.1 "Agent Memory and Rules", §8, §10 K5)
//! as one run of `adk-rust up` with a scripted model: on SQLite (`harness.db`, in CI) and on
//! PostgreSQL (`--store`, `correct_me_then_check_on_postgres`, `#[ignore]`d and run by hand):
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-cli --features scripted-model,audit-sql,postgres --run-ignored only \
//!   --test correct_me_then_check
//! ```
//!
//! | Step | What happens | What holds |
//! |---|---|---|
//! | 1 | Two notes about invoices and a guess are remembered; the kernel's overnight tidy-up runs | The tidy-up is the kernel's own process; "What your agents remember" lists one merged note instead of the two |
//! | 2 | Undo, through the home's route | Both notes are back and the merged one is gone |
//! | 3 | That's right, on the guess | It reads "You said this" (§8.1) |
//! | 4 | Forget it, on one note; then a turn about invoices | The turn recalls the other note and not the forgotten one |
//! | 5 | A rule lets a write go ahead without asking; Stop allowing; the same write again | The first write ran without a decision; the identical one waits for a decision and writes nothing |
//! | 6 | Every action is on record; then one record is changed in the database | The home's check and `adk-rust audit verify` agree, before and after the change |

#![cfg(all(unix, feature = "scripted-model", feature = "persist", feature = "audit-sql"))]

mod common;

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use adk_harness::{
    FactEmbedding, FactKind, FactScope, FactSource, FactStatus, FactStore, MemoryFact, Provenance,
};
use adk_kernel::control::FactOrigin;
use serde_json::{Value, json};

/// The person `common::isolate` runs every command as; the loopback caller of `up`.
const USER: &str = "pty-tester";
/// The app the harness keeps its memory under.
const APP: &str = "adk-rust";

/// `adk-rust up` as a child process, killed when dropped, so a failing test leaves nothing
/// running.
struct Up(Child);

impl Drop for Up {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Where the scenario's facts are kept: `harness.db` in the kernel home, or the shared store.
enum Facts {
    Sqlite(adk_harness::SqliteFactStore, sqlx::SqlitePool),
    #[cfg(feature = "postgres")]
    Postgres(adk_harness::PostgresFactStore, sqlx::PgPool),
}

impl Facts {
    fn store(&self) -> &dyn FactStore {
        match self {
            Self::Sqlite(store, _) => store,
            #[cfg(feature = "postgres")]
            Self::Postgres(store, _) => store,
        }
    }

    /// Closes the connections, so `up` opens `harness.db` alone and turns on WAL mode.
    async fn close(self) {
        match self {
            Self::Sqlite(_, pool) => pool.close().await,
            #[cfg(feature = "postgres")]
            Self::Postgres(_, pool) => pool.close().await,
        }
    }

    async fn get(&self, fact: &MemoryFact) -> MemoryFact {
        self.store().get(&fact.id).await.unwrap().expect("the fact is kept")
    }

    /// Changes the kernel's record of `action` in the database, as someone with access to
    /// the store could.
    async fn tamper(&self, log: &str, action: &str) -> u64 {
        let changed = format!("{action}.rewritten");
        match self {
            Self::Sqlite(_, pool) => sqlx::query(
                "UPDATE adk_audit_entries SET record = replace(record, ?, ?) \
                 WHERE log_id = ? AND record LIKE ?",
            )
            .bind(action)
            .bind(&changed)
            .bind(log)
            .bind(format!("%\"{action}\"%"))
            .execute(pool)
            .await
            .unwrap()
            .rows_affected(),
            #[cfg(feature = "postgres")]
            Self::Postgres(_, pool) => sqlx::query(
                "UPDATE adk_audit_entries SET record = replace(record, $1, $2) \
                 WHERE log_id = $3 AND record LIKE $4",
            )
            .bind(action)
            .bind(&changed)
            .bind(log)
            .bind(format!("%\"{action}\"%"))
            .execute(pool)
            .await
            .unwrap()
            .rows_affected(),
        }
    }
}

/// A note the agents took, user-wide, with a vector in the band the tidy-up merges.
async fn note(store: &dyn FactStore, text: &str, confidence: f32, vector: Vec<f32>) -> MemoryFact {
    let mut fact = MemoryFact::new(
        FactScope::new(APP, USER, None),
        FactKind::Preference,
        text,
        FactStatus::Assumed,
        Provenance::new(FactSource::AgentInferred, "earlier", "inv-0"),
    );
    fact.valid_from = chrono::Utc::now() - chrono::Duration::days(1);
    fact.confidence = Some(confidence);
    let stored = store.put(fact).await.unwrap();
    store.put_embedding(&stored.id, FactEmbedding::new("test/seeded", vector)).await.unwrap();
    stored
}

/// A unit vector at cosine `score` to `[1, 0, 0]`.
fn at_cosine(score: f32) -> Vec<f32> {
    vec![score, (1.0 - score * score).sqrt(), 0.0]
}

/// Where a kernel keeps its home, its workspace and its stores.
struct Place {
    home: tempfile::TempDir,
    ws: tempfile::TempDir,
    kernel: PathBuf,
    port: u16,
    /// `--store`: every store in one shared backend; `None` keeps them in `harness.db`.
    store: Option<String>,
}

impl Place {
    fn new(store: Option<String>) -> Self {
        let home = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let kernel = home.path().join("kernel");
        std::fs::create_dir_all(&kernel).unwrap();
        // The tidy-up is admitted by one new note, at once, and only once in this run.
        let config = common::config_dir(home.path());
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("harness.toml"),
            "[kernel.consolidation]\nmin_new_facts = 1\ndebounce_minutes = 0\nmin_interval_minutes = 600\n",
        )
        .unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        Self { home, ws, kernel, port, store }
    }

    /// The fact store `up` will open, migrated, so the notes are there before it starts.
    async fn facts(&self) -> Facts {
        match &self.store {
            None => {
                let url = format!("sqlite://{}?mode=rwc", self.kernel.join("harness.db").display());
                let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
                let store = adk_harness::SqliteFactStore::new(pool.clone());
                store.migrate().await.unwrap();
                Facts::Sqlite(store, pool)
            }
            #[cfg(feature = "postgres")]
            Some(url) => {
                let pool = sqlx::PgPool::connect(url).await.unwrap();
                let store = adk_harness::PostgresFactStore::new(pool.clone());
                store.migrate().await.unwrap();
                Facts::Postgres(store, pool)
            }
            #[cfg(not(feature = "postgres"))]
            Some(_) => unreachable!("a shared store needs the `postgres` feature"),
        }
    }

    fn api(&self) -> Api {
        Api { base: format!("http://127.0.0.1:{}", self.port), client: reqwest::Client::new() }
    }

    /// `adk-rust up` in Ask first with `script`; returns once it listens.
    fn up(&self, script: &str) -> Up {
        let script_path = self.home.path().join("script.json");
        std::fs::write(&script_path, script).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
        command
            .args(["up", "--home", &self.kernel.display().to_string()])
            .args(["--bind", &format!("127.0.0.1:{}", self.port)])
            .args(["--dir", &self.ws.path().display().to_string()])
            .args(["--model-script", &script_path.display().to_string(), "--mode", "ask"])
            // Recall by shared words, so nothing is embedded or downloaded.
            .args(["--embedder", "lexical"])
            .current_dir(self.ws.path())
            .stdin(Stdio::null());
        if let Some(store) = &self.store {
            command.args(["--store", store]);
        }
        common::isolate(&mut command, self.home.path());
        command.env_remove("TELEGRAM_BOT_TOKEN");
        // What the kernel decided about each tick, and every warning, for a failure to name;
        // the server's log subscriber writes to standard output.
        let log = std::fs::File::create(self.tick_log()).unwrap();
        command
            .env("RUST_LOG", "warn,adk_kernel::consolidation=debug")
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        let mut child = command.spawn().expect("adk-rust starts");
        let started = Instant::now();
        while std::net::TcpStream::connect(("127.0.0.1", self.port)).is_err() {
            if child.try_wait().unwrap().is_some() || started.elapsed() > common::TIMEOUT {
                let _ = child.kill();
                panic!("up did not listen");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Up(child)
    }

    /// Where `up` writes what it decided about each consolidation tick.
    fn tick_log(&self) -> PathBuf {
        self.home.path().join("ticks.log")
    }

    /// The kernel's signed log id and the verifier key `adk-rust audit verify` is given.
    fn log(&self) -> (String, String) {
        use adk_auth::signed_audit::{AuditSigner as _, Ed25519Signer};

        let id = std::fs::read_to_string(self.kernel.join("kernel-id")).unwrap();
        let key = std::fs::read_to_string(self.kernel.join("kernel-audit.key")).unwrap();
        let verifier = Ed25519Signer::from_note_key(key.trim()).unwrap().verifier_key();
        (format!("kernel-{}", id.trim()), verifier)
    }

    /// The kernel's record, exported as `adk-rust audit export` writes it, and the report of
    /// `adk-rust audit verify --json` over it.
    fn audit(&self) -> (Vec<Value>, Value, bool) {
        let (log, verifier) = self.log();
        let database = match &self.store {
            Some(store) => store.clone(),
            None => format!("sqlite://{}", self.kernel.join("harness.db").display()),
        };
        let out = self.home.path().join("record.jsonl");
        let (home, ws) = (self.home.path(), self.ws.path());
        let out_arg = out.display().to_string();
        let export = common::run(
            &["audit", "export", "--database-url", &database, "--log-id", &log, "--out", &out_arg],
            home,
            ws,
        );
        assert!(export.status.success(), "{}", common::text(&export));
        let verified =
            common::run(&["audit", "verify", &out_arg, "--key", &verifier, "--json"], home, ws);
        let report: Value = serde_json::from_slice(&verified.stdout)
            .unwrap_or_else(|_| panic!("audit verify: {}", common::text(&verified)));
        let records = std::fs::read_to_string(&out)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|line| line["type"] == "entry")
            .filter_map(|entry| serde_json::from_str(entry["record"].as_str()?).ok())
            .collect();
        (records, report, verified.status.success())
    }
}

struct Api {
    base: String,
    client: reqwest::Client,
}

impl Api {
    async fn get(&self, path: &str) -> Value {
        let response = self.client.get(format!("{}{path}", self.base)).send().await.unwrap();
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        assert!(status.is_success(), "GET {path}: {status} {body}");
        body
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let response =
            self.client.post(format!("{}{path}", self.base)).json(&body).send().await.unwrap();
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        assert!(status.is_success(), "POST {path}: {status} {body}");
        body
    }

    /// Sends `text` in thread `session` from the web home.
    async fn send(&self, session: &str, text: &str) {
        self.post(
            "/api/surface/commands",
            json!({ "surface": "web", "origin": { "kind": "keyboard" },
                    "command": { "type": "send", "sessionId": session, "text": text } }),
        )
        .await;
    }

    async fn processes(&self) -> Vec<Value> {
        self.get("/api/kernel/processes").await["processes"].as_array().cloned().unwrap_or_default()
    }

    /// The runs of `session`, once there are `count` and none is still to run or running.
    async fn settled(&self, session: &str, count: usize) -> Vec<Value> {
        eventually(&format!("{count} settled runs in {session}"), || async {
            let runs: Vec<Value> = self
                .processes()
                .await
                .into_iter()
                .filter(|process| process["run"]["sessionId"] == session)
                .map(|process| process["run"].clone())
                .collect();
            let done = |run: &Value| {
                matches!(run["state"].as_str(), Some("completed" | "failed" | "cancelled"))
            };
            (runs.len() >= count && runs.iter().all(done)).then_some(runs)
        })
        .await
    }

    /// What your agents remember: the live facts' ids and the tidy-ups.
    async fn memory(&self) -> (Vec<String>, Vec<Value>) {
        let memory = self.get("/api/kernel/memory").await;
        let ids = memory["facts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|fact| fact["id"].as_str().unwrap().to_string())
            .collect();
        (ids, memory["tidyUps"].as_array().cloned().unwrap_or_default())
    }

    async fn decisions(&self) -> Vec<Value> {
        self.get("/api/surface/decisions?surface=web").await["decisions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = Instant::now();
    while started.elapsed() <= Duration::from_secs(60) {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("timed out waiting for {what}");
}

/// The ids of `facts`, sorted.
fn sorted(facts: &[&MemoryFact]) -> Vec<String> {
    let mut ids: Vec<String> = facts.iter().map(|fact| fact.id.clone()).collect();
    ids.sort();
    ids
}

/// The scenario on `store` (`None`: SQLite in `harness.db`).
async fn correct_me_then_check_on(store: Option<String>) {
    let place = Place::new(store);
    let api = place.api();
    let ws = place.ws.path();

    // Two notes about when invoices go out, close enough to merge, and an unrelated guess.
    let facts = place.facts().await;
    let first =
        note(facts.store(), "Invoices go out on the last working day.", 0.6, at_cosine(1.0)).await;
    let second = note(
        facts.store(),
        "Send the invoices on the last working day of the month.",
        0.8,
        at_cosine(0.85),
    )
    .await;
    let guess =
        note(facts.store(), "You prefer email to phone calls.", 0.7, vec![0.0, 0.0, 1.0]).await;

    // The model: the tidy-up's merge, the reply to the turn about invoices, then the rule
    // thread's write, its answer, and the identical write.
    let write = json!({ "call": { "name": "write_file", "args": {
        "path": "notes/checked.txt", "content": "Invoices checked."
    } } });
    let script = json!([
        { "text": json!({
            "merged_text": "Invoices go out on the last working day of the month.",
            "source_ids": [first.id, second.id],
            "supersedes": [first.id, second.id]
        }).to_string() },
        { "text": "On the last working day of the month." },
        write,
        { "text": "Saved." },
        write,
        { "text": "Done." },
        { "text": "Done." }
    ]);
    facts.close().await;
    let up = place.up(&script.to_string());
    let started = Instant::now();
    let facts = place.facts().await;

    // 1. The kernel's overnight tidy-up merges the two notes into one. The kernel weighs a tick
    // once a minute and holds it while another of its processes runs, such as the first daily
    // `/proc` pass, so it may come a minute or two after the start.
    let mut tidied = None;
    while tidied.is_none() && started.elapsed() < Duration::from_secs(210) {
        let (ids, tidy_ups) = api.memory().await;
        if tidy_ups.first().is_some_and(|tick| tick["finishedAt"].is_string()) {
            tidied = Some((ids, tidy_ups));
        } else {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    let Some((ids, tidy_ups)) = tidied else {
        let ran: Vec<String> = api
            .processes()
            .await
            .iter()
            .map(|process| {
                let run = &process["run"];
                format!("{} {} {}", run["target"], run["state"], run["error"])
            })
            .collect();
        let decided = std::fs::read_to_string(place.tick_log()).unwrap_or_default();
        panic!("no overnight tidy-up after {:?}; processes {ran:?}\n{decided}", started.elapsed());
    };
    let tick = &tidy_ups[0];
    assert_eq!(
        (tick["merged"].as_u64(), tick["superseded"].as_u64()),
        (Some(1), Some(2)),
        "{tick}"
    );
    let tick_id = tick["id"].as_str().unwrap().to_string();
    assert!(
        api.processes().await.iter().any(|process| {
            process["run"]["target"] == adk_kernel::CONSOLIDATION_TARGET
                && process["run"]["state"] == "completed"
        }),
        "the tidy-up ran as the kernel's own process"
    );
    let merged: Vec<&String> =
        ids.iter().filter(|id| ![&first.id, &second.id, &guess.id].contains(id)).collect();
    assert_eq!(merged.len(), 1, "one merged note: {ids:?}");
    let merged = merged[0].clone();
    assert!(!ids.contains(&first.id) && !ids.contains(&second.id), "{ids:?}");
    let mut sources = facts.store().get(&merged).await.unwrap().unwrap().provenance.source_ids;
    sources.sort();
    assert_eq!(sources, sorted(&[&first, &second]), "the merged note names both");

    // 2. Undo, through the home's route: both notes are back, the merged one is gone.
    let undone = api.post("/api/kernel/memory/undo", json!({ "tidyUp": tick_id })).await;
    let mut restored: Vec<String> = undone["restored"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_string())
        .collect();
    restored.sort();
    assert_eq!(restored, sorted(&[&first, &second]), "{undone}");
    assert_eq!(undone["forgotten"], json!([merged]), "{undone}");
    let (ids, tidy_ups) = api.memory().await;
    assert!(ids.contains(&first.id) && ids.contains(&second.id) && !ids.contains(&merged));
    assert!(tidy_ups[0]["undoneAt"].is_string(), "{tidy_ups:?}");

    // 3. That's right: the guess reads "You said this" (§8.1).
    let confirmed =
        api.post(&format!("/api/kernel/memory/facts/{}/confirm", guess.id), json!({})).await;
    let origin: FactOrigin = serde_json::from_value(confirmed["fact"]["origin"].clone()).unwrap();
    assert_eq!(origin.label(), "You said this", "{confirmed}");
    let listed = api.get("/api/kernel/memory").await;
    let shown =
        listed["facts"].as_array().unwrap().iter().find(|fact| fact["id"] == guess.id.as_str());
    let shown: FactOrigin = serde_json::from_value(shown.unwrap()["origin"].clone()).unwrap();
    assert_eq!(shown.label(), "You said this");
    assert_eq!(facts.get(&guess).await.status, FactStatus::Known);

    // 4. Forget it: the next turn about invoices recalls the other note, not this one.
    api.post(&format!("/api/kernel/memory/facts/{}/forget", first.id), json!({})).await;
    assert!(!api.memory().await.0.contains(&first.id));
    api.send("invoices", "invoices last working day").await;
    api.settled("invoices", 1).await;
    assert!(facts.get(&second).await.last_recalled.is_some(), "the kept note is recalled");
    assert_eq!(facts.get(&first).await.last_recalled, None, "the forgotten note is not");

    // 5. Allowed without asking: a rule lets the write go ahead.
    let rule = api.post("/api/rules", json!({ "tool": "write_file", "pathWithin": "notes" })).await;
    let rule_id = rule["rule"]["id"].as_str().unwrap().to_string();
    api.send("rules", "Note that the invoices are checked.").await;
    api.settled("rules", 1).await;
    assert!(ws.join("notes/checked.txt").exists(), "the write went ahead");
    assert!(
        !api.decisions().await.iter().any(|card| card["sessionId"] == "rules"),
        "nothing waited"
    );
    std::fs::remove_file(ws.join("notes/checked.txt")).unwrap();

    // The kernel signs its first checkpoint with the first record a minute after its log
    // opened; past that minute, Stop allowing is signed by then at the latest, and every
    // action before it.
    tokio::time::sleep(Duration::from_secs(62).saturating_sub(started.elapsed())).await;

    // Stop allowing: the identical write now waits for a decision and writes nothing.
    let response =
        api.client.delete(format!("{}/api/rules/{rule_id}", api.base)).send().await.unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    api.send("rules", "Note that the invoices are checked.").await;
    let waiting = eventually("the identical write, waiting", || async {
        api.decisions()
            .await
            .into_iter()
            .find(|card| card["sessionId"] == "rules" && card["call"]["tool"] == "write_file")
    })
    .await;
    assert_eq!(waiting["call"]["args"]["path"], "notes/checked.txt", "{waiting}");
    assert!(!ws.join("notes/checked.txt").exists(), "nothing is written before a person says yes");

    // 6. Every action is on record: the home's check and `adk-rust audit verify` agree.
    let check = api.get("/api/kernel/record/check").await;
    let (records, report, verified) = place.audit();
    assert!(check["valid"] == true && verified, "{check} {report}");
    assert_eq!(report["failures"], json!([]), "{report}");
    let actions: Vec<&str> =
        records.iter().filter_map(|record| record["action"].as_str()).collect();
    for action in ["harness.memory.consolidated", "harness.rule.revoked"] {
        assert!(actions.contains(&action), "{action} on record: {actions:?}");
    }
    let controls: Vec<&str> = records
        .iter()
        .filter(|record| record["action"] == "kernel.control")
        .filter_map(|record| record["ext"]["action"].as_str())
        .collect();
    for action in ["memory.undo", "memory.confirm", "memory.forget"] {
        assert!(controls.contains(&action), "{action} on record: {controls:?}");
    }
    // The turn about invoices recalled the kept note and not the forgotten one.
    let recalled: Vec<&Value> =
        records.iter().filter(|record| record["action"] == "harness.memory.recalled").collect();
    assert_eq!(recalled.len(), 1, "{recalled:?}");
    let recalled = recalled[0]["ext"]["factIds"].as_array().unwrap();
    assert!(recalled.contains(&json!(second.id)) && !recalled.contains(&json!(first.id)));

    // Someone rewrites the record of the tidy-up in the database: both say so.
    let (log, _) = place.log();
    assert_eq!(facts.tamper(&log, "harness.memory.consolidated").await, 1);
    let check = api.get("/api/kernel/record/check").await;
    let (_, report, verified) = place.audit();
    assert!(check["valid"] == false && !verified, "{check} {report}");
    assert!(check["problem"].is_string(), "{check}");
    let kinds = |report: &Value| {
        let mut kinds: Vec<String> = report["failures"]
            .as_array()
            .unwrap()
            .iter()
            .map(|failure| failure["kind"].to_string())
            .collect();
        kinds.sort();
        kinds.dedup();
        kinds
    };
    assert_eq!(kinds(&check["report"]), kinds(&report), "{check} {report}");
    drop(up);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn correct_me_then_check() {
    correct_me_then_check_on(None).await;
}

/// The scenario with every store on PostgreSQL, in a database of its own.
#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL, a PostgreSQL server; CI's postgres job runs it"]
async fn correct_me_then_check_on_postgres() {
    let server = std::env::var("DATABASE_URL").expect("DATABASE_URL names a PostgreSQL server");
    let admin = sqlx::PgPool::connect(&server).await.unwrap();
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    // A name of digits and underscores only, so it needs no quoting.
    let database = format!("correct_me_then_check_{nanos}");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    correct_me_then_check_on(Some(with_database(&server, &database))).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)")))
        .execute(&admin)
        .await
        .unwrap();
}

/// `server` with its database path replaced by `database`, its query kept.
#[cfg(feature = "postgres")]
fn with_database(server: &str, database: &str) -> String {
    let (base, query) = server.split_once('?').unwrap_or((server, ""));
    let host = base.find("://").map_or(0, |at| at + 3);
    let root = base[host..].find('/').map_or(base, |at| &base[..host + at]);
    match query {
        "" => format!("{root}/{database}"),
        query => format!("{root}/{database}?{query}"),
    }
}
