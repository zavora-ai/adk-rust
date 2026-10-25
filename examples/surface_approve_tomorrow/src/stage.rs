//! The stage both phase scenarios of spec 3.3 §12 run on: the `adk-rust up` binary as a real
//! child process in a private home, its store (SQLite in the home, or a fresh PostgreSQL
//! database), the model it runs on, and the transcript and checks a run prints.
//!
//! `surface_morning_after` includes this file with `#[path]`, so both examples drive the
//! kernel the same way.
//!
//! Every program a scenario starts is stopped on every exit path: the kernel by
//! [`Kernel`]'s `Drop` (`SIGKILL`, then reaped), one-shot commands by `kill_on_drop`, and
//! the mail driver the kernel starts exits when its parent is gone ([`crate::mail`]).

// Shared by both scenarios, each of which uses part of it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use adk_kernel::control::ControlClient;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, Utc};
use serde_json::Value;

/// The provider live mode runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// `gemini-3.5-flash-lite`, with `GOOGLE_API_KEY` or `GEMINI_API_KEY`.
    Gemini,
    /// `claude-sonnet-5-5`, with `ANTHROPIC_API_KEY`.
    Anthropic,
}

impl Provider {
    /// `adk-rust --provider`'s value.
    pub fn name(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::Anthropic => "anthropic",
        }
    }

    /// The model it runs.
    pub fn model(self) -> &'static str {
        match self {
            Self::Gemini => "gemini-3.5-flash-lite",
            Self::Anthropic => "claude-sonnet-5-5",
        }
    }

    /// The environment variables its key may be in.
    fn keys(self) -> &'static [&'static str] {
        match self {
            Self::Gemini => &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
            Self::Anthropic => &["ANTHROPIC_API_KEY"],
        }
    }
}

/// Where the scenario's stores live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    /// `harness.db` in the kernel home.
    Sqlite,
    /// One PostgreSQL database every store shares (`up --store`), created for the run on
    /// the server `DATABASE_URL` names and dropped after it.
    Postgres,
}

/// The model the kernel runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    /// `adk-rust up --model-script`: replies the scenario wrote, no key needed.
    Scripted,
    /// A live model.
    Live(Provider),
}

/// The command line of a scenario.
#[derive(Debug, Clone)]
pub struct Options {
    /// Where the stores live.
    pub store: StoreKind,
    /// The model.
    pub model: ModelKind,
    /// The `adk-rust` binary.
    pub bin: PathBuf,
    /// Keep the scenario's folder and database afterwards.
    pub keep: bool,
}

impl Options {
    /// Reads `--store sqlite|postgres`, `--live` (Gemini), `--provider gemini|anthropic`
    /// (live on that provider), `--bin PATH` and `--keep`; the binary defaults to
    /// `$ADK_RUST_BIN`, else the workspace's `target/debug/adk-rust`.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut store = StoreKind::Sqlite;
        let mut model = ModelKind::Scripted;
        let mut bin = std::env::var_os("ADK_RUST_BIN").map(PathBuf::from);
        let mut keep = false;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--store" => {
                    store = match args.next().as_deref() {
                        Some("sqlite") => StoreKind::Sqlite,
                        Some("postgres") => StoreKind::Postgres,
                        other => bail!("--store takes sqlite or postgres, not {other:?}"),
                    }
                }
                "--live" => {
                    if model == ModelKind::Scripted {
                        model = ModelKind::Live(Provider::Gemini);
                    }
                }
                "--provider" => {
                    model = match args.next().as_deref() {
                        Some("gemini") => ModelKind::Live(Provider::Gemini),
                        Some("anthropic") => ModelKind::Live(Provider::Anthropic),
                        other => bail!("--provider takes gemini or anthropic, not {other:?}"),
                    }
                }
                "--bin" => bin = Some(args.next().context("--bin takes a path")?.into()),
                "--keep" => keep = true,
                other => bail!(
                    "unknown argument {other}; use --store sqlite|postgres, --live, \
                     --provider gemini|anthropic, --bin PATH, --keep"
                ),
            }
        }
        let bin = bin.unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/adk-rust")
        });
        if !bin.exists() {
            bail!(
                "no adk-rust binary at {}: build it from the workspace root with \
                 `cargo build -p adk-cli --bin adk-rust --features scripted-model,tui,postgres`, \
                 or pass --bin or ADK_RUST_BIN",
                bin.display()
            );
        }
        if let ModelKind::Live(provider) = model {
            load_keys();
            let has_key = provider
                .keys()
                .iter()
                .any(|key| std::env::var(key).is_ok_and(|value| !value.trim().is_empty()));
            if !has_key {
                bail!("a live run on {} needs {}", provider.model(), provider.keys().join(" or "));
            }
        }
        Ok(Self { store, model, bin, keep })
    }

    /// The store and model in words, for the banner.
    pub fn describe(&self) -> String {
        let store = match self.store {
            StoreKind::Sqlite => "SQLite (harness.db in the kernel home)",
            StoreKind::Postgres => "PostgreSQL (a fresh database on DATABASE_URL's server)",
        };
        let model = match self.model {
            ModelKind::Scripted => "a scripted model (no key)".to_string(),
            ModelKind::Live(provider) => format!("{} (live)", provider.model()),
        };
        format!("store {store} · model {model}")
    }
}

/// Loads keys from `$SCENARIO_ENV_FILE`, else the repository's `.env`, over the environment.
/// Nothing is printed: keys only reach the kernel, which inherits them.
fn load_keys() {
    let file = std::env::var_os("SCENARIO_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env"));
    if let Ok(file) = file.canonicalize() {
        let _ = dotenvy::from_path_override(file);
    }
}

/// The scenario's store: nothing for SQLite, a database of its own on PostgreSQL.
pub struct Store {
    kind: StoreKind,
    /// The server, and the database created on it.
    postgres: Option<(String, String)>,
    url: Option<String>,
}

impl Store {
    /// Opens the store `kind`: on PostgreSQL, creates the database `name` on the server of
    /// `DATABASE_URL`.
    pub async fn open(kind: StoreKind, name: &str) -> Result<Self> {
        match kind {
            StoreKind::Sqlite => Ok(Self { kind, postgres: None, url: None }),
            StoreKind::Postgres => {
                use sqlx::Connection;
                let server = std::env::var("DATABASE_URL").context(
                    "--store postgres needs DATABASE_URL, a PostgreSQL server the scenario may \
                     create a database on, such as postgres://postgres@127.0.0.1:5432/postgres",
                )?;
                let database = format!("{name}_{}", std::process::id());
                let mut admin = sqlx::postgres::PgConnection::connect(&server)
                    .await
                    .context("could not connect to DATABASE_URL")?;
                // The name is the scenario's own and a process id: nothing a person typed.
                sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE IF EXISTS \"{database}\"")))
                    .execute(&mut admin)
                    .await?;
                sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{database}\"")))
                    .execute(&mut admin)
                    .await?;
                admin.close().await?;
                let mut url = reqwest::Url::parse(&server).context("DATABASE_URL is not a URL")?;
                url.set_path(&format!("/{database}"));
                Ok(Self { kind, postgres: Some((server, database)), url: Some(url.into()) })
            }
        }
    }

    /// `up --store`'s value, when the stores are shared.
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    /// The store in words, without credentials.
    pub fn label(&self) -> String {
        match (&self.kind, &self.postgres) {
            (StoreKind::Postgres, Some((_, database))) => format!("PostgreSQL database {database}"),
            _ => "SQLite, harness.db in the kernel home".to_string(),
        }
    }

    /// Drops the PostgreSQL database, unless `keep`.
    pub async fn close(self, keep: bool) -> Result<()> {
        use sqlx::Connection;
        if let (Some((server, database)), false) = (self.postgres, keep) {
            let mut admin = sqlx::postgres::PgConnection::connect(&server).await?;
            let drop = format!("DROP DATABASE IF EXISTS \"{database}\" WITH (FORCE)");
            sqlx::query(sqlx::AssertSqlSafe(drop)).execute(&mut admin).await?;
            admin.close().await?;
        }
        Ok(())
    }
}

/// A scenario's private world: a folder with the person's `HOME`, the kernel home, the mail
/// driver's outbox and the kernel's logs; the port the kernel serves on; the binary.
pub struct Stage {
    root: tempfile::TempDir,
    /// The person's `HOME`: their user config lives under it.
    pub user: PathBuf,
    /// The kernel home: its socket, `harness.db` and signed audit log.
    pub home: PathBuf,
    /// Where the mail driver keeps what it sent.
    pub mail: PathBuf,
    /// The kernel's HTTP port, the same across restarts.
    pub port: u16,
    /// The `adk-rust` binary.
    pub bin: PathBuf,
    /// The model.
    pub model: ModelKind,
    /// The store.
    pub store: Store,
    boots: std::cell::Cell<u32>,
}

impl Stage {
    /// A fresh stage for `options`, its stores named after `scenario`.
    pub async fn new(options: &Options, scenario: &str) -> Result<Self> {
        let root = tempfile::Builder::new().prefix(&format!("{scenario}-")).tempdir()?;
        // The real path: the OS sandbox resolves symbolic links such as macOS's /var.
        let path = root.path().canonicalize()?;
        let user = path.join("user");
        let home = path.join("kernel");
        let mail = path.join("mail");
        for dir in [&user, &home, &mail] {
            std::fs::create_dir_all(dir)?;
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        let store = Store::open(options.store, scenario).await?;
        Ok(Self {
            root,
            user,
            home,
            mail,
            port,
            bin: options.bin.clone(),
            model: options.model,
            store,
            boots: std::cell::Cell::new(0),
        })
    }

    /// The scenario's folder.
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// Keeps the folder after the run, for inspection.
    pub fn keep(self) -> (PathBuf, Store) {
        let Self { root, store, .. } = self;
        (root.keep(), store)
    }

    /// Ends the run: removes the folder and, unless `keep`, the database.
    pub async fn close(self, keep: bool) -> Result<Option<PathBuf>> {
        if keep {
            let (root, store) = self.keep();
            store.close(true).await?;
            return Ok(Some(root));
        }
        let Self { root, store, .. } = self;
        drop(root);
        store.close(false).await?;
        Ok(None)
    }

    /// Writes the person's user config, `$HOME/<config dir>/adk-rust/harness.toml`, on macOS
    /// and on Linux.
    pub fn write_user_config(&self, toml: &str) -> Result<()> {
        for dir in ["Library/Application Support/adk-rust", ".config/adk-rust"] {
            let dir = self.user.join(dir);
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("harness.toml"), toml)?;
        }
        Ok(())
    }

    /// The personal preset's first-run answers, kept in the kernel home, so `up` asks
    /// nothing (spec 3.3 §10 H1, §13 Q7).
    pub fn write_personal_answers(
        &self,
        quiet_time: Option<&str>,
        daily_limit_usd: f64,
    ) -> Result<()> {
        let mut answers = format!("daily_spending_limit_usd = {daily_limit_usd:.2}\n");
        if let Some(quiet_time) = quiet_time {
            answers.push_str(&format!("quiet_time = \"{quiet_time}\"\n"));
        }
        std::fs::write(self.home.join("personal.toml"), answers)?;
        Ok(())
    }

    /// The kernel's HTTP address.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The private environment of every program the scenario starts: its own `HOME` and
    /// config directory, so nothing reads or writes the person's real ones.
    fn isolate(&self, command: &mut Command) {
        command
            .env("HOME", &self.user)
            .env("XDG_CONFIG_HOME", self.user.join(".config"))
            .env("RUST_LOG", std::env::var("SCENARIO_LOG").unwrap_or_else(|_| "warn".into()))
            .env_remove("ADK_SERVE_TOKEN")
            .env_remove("TELEGRAM_BOT_TOKEN")
            .env_remove("TELEGRAM_API_BASE");
    }

    /// Runs `adk-rust <args>` once, isolated, and returns whether it succeeded and what it
    /// printed. It is killed if it runs past `limit`.
    pub async fn run_cli(&self, args: &[&str], limit: Duration) -> Result<(bool, String)> {
        let mut command = Command::new(&self.bin);
        command.args(args).stdin(Stdio::null());
        self.isolate(&mut command);
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let output = tokio::time::timeout(limit, command.output())
            .await
            .with_context(|| format!("adk-rust {} ran past {limit:?}", args.join(" ")))??;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok((output.status.success(), text))
    }

    /// The same as [`run_cli`](Self::run_cli), returning standard output and the exit code.
    pub async fn run_cli_stdout(&self, args: &[&str], limit: Duration) -> Result<(i32, String)> {
        let mut command = Command::new(&self.bin);
        command.args(args).stdin(Stdio::null());
        self.isolate(&mut command);
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let output = tokio::time::timeout(limit, command.output())
            .await
            .with_context(|| format!("adk-rust {} ran past {limit:?}", args.join(" ")))??;
        Ok((output.status.code().unwrap_or(-1), String::from_utf8_lossy(&output.stdout).into()))
    }

    /// The kernel's control plane on its home's socket, as `adk-rust ps` reaches it.
    pub async fn client(&self) -> Result<ControlClient> {
        ControlClient::connect_local(&self.home).await?.context("the kernel's socket is not open")
    }

    /// Starts `adk-rust up` on this stage: Ask, the personal preset, no long-term memory, the
    /// stage's store, and `model`. `env` adds to the kernel's environment. Returns once its
    /// socket and HTTP control plane answer.
    pub async fn start_kernel(&self, model: Model<'_>, env: &[(&str, String)]) -> Result<Kernel> {
        let boot = self.boots.get() + 1;
        self.boots.set(boot);
        let log_path = self.root.path().join(format!("kernel-{boot}.log"));
        let log = std::fs::File::create(&log_path)?;
        let mut command = Command::new(&self.bin);
        command
            .arg("up")
            .arg("--home")
            .arg(&self.home)
            .args(["--bind", &format!("127.0.0.1:{}", self.port)])
            .args(["--mode", "ask", "--preset", "personal", "--no-memory", "--no-consolidation"])
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        if let Some(url) = self.store.url() {
            command.args(["--store", url]);
        }
        match (self.model, model) {
            (ModelKind::Scripted, model) => {
                let replies = match model {
                    Model::Answers(replies) => replies,
                    Model::Unreachable => &[],
                };
                let script = self.root.path().join(format!("script-{boot}.json"));
                std::fs::write(&script, serde_json::to_vec_pretty(replies)?)?;
                command.arg("--model-script").arg(script);
            }
            (ModelKind::Live(provider), model) => {
                command.args(["--provider", provider.name(), "--model", provider.model()]);
                if let Model::Unreachable = model {
                    command.args(["--api-key", REFUSED_KEY]);
                }
            }
        }
        self.isolate(&mut command);
        for (key, value) in env {
            command.env(key, value);
        }
        let child =
            command.spawn().with_context(|| format!("failed to start {}", self.bin.display()))?;
        let mut kernel = Kernel { child: Some(child), log: log_path };
        let started = Instant::now();
        let http = reqwest::Client::new();
        loop {
            if let Some(status) = kernel.try_wait()? {
                bail!("adk-rust up exited with {status}:\n{}", kernel.log());
            }
            if let Some(client) = ControlClient::connect_local(&self.home).await.ok().flatten()
                && client.health().await.is_ok()
                && http
                    .get(format!("{}/api/kernel/health", self.base_url()))
                    .send()
                    .await
                    .is_ok_and(|response| response.status().is_success())
            {
                return Ok(kernel);
            }
            if started.elapsed() > Duration::from_secs(90) {
                bail!("adk-rust up did not answer within 90 seconds:\n{}", kernel.log());
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }
}

/// A key the provider refuses, for a kernel whose model calls fail.
const REFUSED_KEY: &str = "not-a-valid-key";

/// What the kernel's model does on one start.
#[derive(Debug, Clone, Copy)]
pub enum Model<'a> {
    /// Answers: these replies in scripted mode, the live model otherwise.
    Answers(&'a [Value]),
    /// Fails every call, as a model that cannot be reached does: an empty script, or the
    /// live provider with a key it refuses.
    Unreachable,
}

/// `adk-rust up` as a child process. Dropping it kills it with `SIGKILL` and reaps it, so a
/// failed or interrupted scenario leaves no kernel behind.
pub struct Kernel {
    child: Option<Child>,
    log: PathBuf,
}

impl Kernel {
    /// The kernel's process id.
    pub fn pid(&self) -> u32 {
        self.child.as_ref().map_or(0, Child::id)
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        match &mut self.child {
            Some(child) => Ok(child.try_wait()?),
            None => Ok(None),
        }
    }

    /// What the kernel printed.
    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// `kill -9 <pid>`, as a crash or a power cut would end it: no drain, no shutdown hook.
    /// Returns the signal that ended it.
    pub fn kill_9(mut self) -> Result<Option<i32>> {
        let mut child = self.child.take().context("the kernel already ended")?;
        let status = Command::new("kill").args(["-9", &child.id().to_string()]).status()?;
        if !status.success() {
            let _ = child.kill();
        }
        let status = child.wait()?;
        #[cfg(unix)]
        return Ok(std::os::unix::process::ExitStatusExt::signal(&status));
        #[cfg(not(unix))]
        return Ok(status.code());
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Whether any process whose command line contains `marker` is still running.
pub fn running(marker: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", marker])
        .output()
        .is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
}

/// Waits until `check` holds, polling every 200 ms, for at most `limit`.
pub async fn until<F, Fut>(limit: Duration, mut check: F) -> Result<bool>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let deadline = Instant::now() + limit;
    loop {
        if check().await? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Sleeps until `at`.
pub async fn sleep_until(at: DateTime<Utc>) {
    if let Ok(wait) = (at - Utc::now()).to_std() {
        tokio::time::sleep(wait).await;
    }
}

/// `at` as local `HH:MM:SS`.
pub fn clock(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local).format("%H:%M:%S").to_string()
}

/// `at` as local `HH:MM:SS`, the form `[kernel] quiet_hours` accepts.
pub fn quiet_bound(at: DateTime<Utc>) -> String {
    clock(at)
}

/// A cron expression (UTC, with seconds) that fires at `at`, whole seconds, once a day.
pub fn daily_cron(at: DateTime<Utc>) -> String {
    use chrono::Timelike;
    format!("{} {} {} * * *", at.second(), at.minute(), at.hour())
}

/// `at` rounded up to the next whole second.
pub fn whole_second(at: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::{SubsecRound, TimeDelta};
    let floor = at.trunc_subsecs(0);
    if floor == at { at } else { floor + TimeDelta::seconds(1) }
}

/// One named pass condition.
struct Check {
    name: String,
    passed: bool,
    detail: String,
}

/// The run's transcript and checks: each line says, in plain words, what happened at which
/// time of the story, then which pass conditions held.
#[derive(Default)]
pub struct Report {
    checks: Vec<Check>,
}

impl Report {
    /// One line of the transcript at `story` time (real time `at`).
    pub fn say(&self, story: &str, at: DateTime<Utc>, text: &str) {
        println!("  {story:>5}  ({})  {text}", clock(at));
    }

    /// A detail under the last line.
    pub fn note(&self, text: &str) {
        for line in text.lines() {
            println!("                    {line}");
        }
    }

    /// Records the pass condition `name`.
    pub fn check(&mut self, name: &str, passed: bool, detail: impl Into<String>) {
        let detail = detail.into();
        println!("                    {} {name}", if passed { "pass" } else { "FAIL" });
        if !passed && !detail.is_empty() {
            self.note(&format!("  {detail}"));
        }
        self.checks.push(Check { name: name.to_string(), passed, detail });
    }

    /// Whether every check passed.
    pub fn passed(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|check| check.passed)
    }

    /// Prints the verdict and returns the exit code: 0 when every pass condition held.
    pub fn finish(&self, title: &str) -> ExitCode {
        println!();
        let failed: Vec<&Check> = self.checks.iter().filter(|check| !check.passed).collect();
        println!(
            "{title}: {} of {} pass conditions held",
            self.checks.len() - failed.len(),
            self.checks.len()
        );
        for check in &failed {
            println!("  failed: {}", check.name);
            if !check.detail.is_empty() {
                println!("          {}", check.detail);
            }
        }
        if self.passed() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
    }
}

/// `GET url` as JSON, with `token` as a bearer token when given.
pub async fn get_json(url: &str, token: Option<&str>) -> Result<(u16, Value)> {
    let mut request = reqwest::Client::new().get(url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let body = response.json::<Value>().await.unwrap_or(Value::Null);
    Ok((status, body))
}

/// `POST url` with the JSON `body`, with `token` as a bearer token when given.
pub async fn post_json(url: &str, token: Option<&str>, body: &Value) -> Result<(u16, Value)> {
    let mut request = reqwest::Client::new().post(url).json(body);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let body = response.json::<Value>().await.unwrap_or(Value::Null);
    Ok((status, body))
}
