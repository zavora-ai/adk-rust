//! A website on local fixtures for the `website` feature's tests (spec 3.5 §7.10, §7.11):
//! the bakery's content on a `DirectoryHost`, its two partitions in a kernel partition table,
//! in-memory facts, charters, autonomy records, signals and standing rules, and harnesses
//! built with `website_back_office` and `website_public` over scripted models.

#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;

use adk_auth::signed_audit::NoteVerifier;
use adk_core::audit_log::{AuditActor, InMemoryAuditLogStore};
use adk_core::principal::SubjectRef;
use adk_harness::website::{
    BackOfficeKit, JobBinding, LadderCompiler, PublicKit, SitePartitions, SiteRef, SiteRuntime,
    SiteRuntimeBuilder, SiteSearch,
};
use adk_harness::{
    FactKind, FactSource, FactStatus, FactStore, Harness, HarnessAgent, HarnessAgentBuilder,
    InMemoryFactStore, InMemoryStandingRuleStore, MemoryFact, Provenance,
};
use adk_kernel::charter::{
    AutonomyLadder, CharterStore, CommitDecision, InMemoryAutonomyStore, InMemoryCharterStore,
    InMemorySignalStore, OwnerDecision, RoleApproval,
};
use adk_kernel::{InMemoryAgendaStore, Partitions};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_session::{InMemorySessionService, SessionService};
use adk_site::content::{
    ContentStore, FsContentStore, InMemoryContentHistory, InMemoryContentStore, SiteProject,
};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::DirectoryHost;
use adk_site::log::SiteLog;
use adk_site::signer::{Ed25519ReleaseSigner, ReleaseSigner};
use awp_types::{Charter, Role};
use serde_json::json;

use crate::common::no_user_file;

/// The site's owner, who holds the owner and editor roles.
pub const OWNER: &str = "amina";
/// The publisher of a two-person chain.
pub const PUBLISHER: &str = "baraka";

/// The bakery's charter: `amina` owns and edits, `baraka` publishes; `chains` adds a
/// publish chain of editor and publisher.
pub fn charter(site: &SiteRef, publish_chain: bool) -> Charter {
    let mut chains = vec![json!({ "action": "charter_change", "roles": ["owner"] })];
    if publish_chain {
        chains.push(json!({ "action": "publish", "roles": ["editor", "publisher"] }));
    }
    serde_json::from_value(json!({
        "version": 1,
        "siteId": site.id,
        "kind": "business",
        "governance": {
            "roles": [
                { "principal": OWNER, "roles": ["owner", "editor"] },
                { "principal": PUBLISHER, "roles": ["publisher"] }
            ],
            "approvalChains": chains
        },
        "mission": "Fresh bread for Nakuru every morning.",
        "goals": [],
        "kpis": [],
        "authority": [
            { "job": "freshness", "level": "propose" },
            { "job": "launch", "level": "act_with_approval" },
            { "job": "running-costs", "level": "act_within_budget", "limits": { "spendCap": 5000 } }
        ],
        "budget": { "period": "monthly", "currency": "KES", "caps": { "hosting": 150000 } },
        "voice": { "tone": "Warm", "languages": ["en"], "disclosure": "Written by Juniper Bakery's assistant, an AI agent." },
        "reporting": { "review": "weekly" },
        "public": { "usage": "train-ai=n, search=y" }
    }))
    .expect("the bakery's charter parses")
}

/// The owner's decision `id`.
pub fn owner_decision(id: &str) -> OwnerDecision {
    OwnerDecision::new(id, SubjectRef::human(OWNER), chrono::Utc::now())
}

/// The bakery's jobs: the Steward launches and keeps pages fresh, Guardian rolls back,
/// Finance pays running costs.
pub fn jobs() -> Vec<JobBinding> {
    vec![
        JobBinding::new(
            "launch",
            "steward",
            [
                "content_*",
                "translation_draft",
                "media_intake",
                "release_preview",
                "site_publish",
                "site_discover",
                "interview_record",
                "charter_*",
                "job_promotion_request",
            ],
        ),
        JobBinding::new(
            "freshness",
            "growth",
            ["content_draft", "content_edit", "content_correct", "site_publish"],
        ),
        JobBinding::new("incidents", "guardian", ["site_rollback", "host_fetch"])
            .in_area("run-and-protect"),
        JobBinding::new("running-costs", "finance", ["wallet_pay"]),
    ]
}

/// The bakery on local fixtures.
pub struct World {
    pub site: SiteRef,
    pub runtime: Arc<SiteRuntime>,
    pub partitions: SitePartitions,
    pub table: Partitions,
    pub facts: Arc<InMemoryFactStore>,
    pub charters: Arc<InMemoryCharterStore>,
    pub autonomy: Arc<InMemoryAutonomyStore>,
    pub ladder: AutonomyLadder,
    pub compiler: LadderCompiler,
    pub signals: Arc<InMemorySignalStore>,
    pub agenda: Arc<InMemoryAgendaStore>,
    pub rules: Arc<InMemoryStandingRuleStore>,
    pub store: Arc<InMemoryContentStore>,
    pub search: Arc<SiteSearch>,
    pub sessions: Arc<dyn SessionService>,
    pub host: Arc<DirectoryHost>,
    pub release_key: NoteVerifier,
    pub dirs: Vec<tempfile::TempDir>,
}

pub fn fixture_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/website")
}

impl World {
    /// The bakery with its launch content in the working copy, nothing published yet, and
    /// charter v1 committed when `with_charter`.
    pub async fn new(with_charter: bool, publish_chain: bool) -> Self {
        Self::new_with(with_charter, publish_chain, |runtime| runtime).await
    }

    /// [`new`](Self::new) with `configure` applied to the site runtime's builder.
    pub async fn new_with(
        with_charter: bool,
        publish_chain: bool,
        configure: impl FnOnce(SiteRuntimeBuilder) -> SiteRuntimeBuilder,
    ) -> Self {
        let site = SiteRef::new(uuid::Uuid::new_v4(), "bakery", "https://bakery.example");
        let fixture = fixture_dir();
        let mut base = SiteProject::load(&fixture, chrono::Utc::now()).await.unwrap();
        base.items.clear();
        let store = Arc::new(InMemoryContentStore::default());
        for item in FsContentStore::new(&fixture).list().await.unwrap() {
            store.put(&item).await.unwrap();
        }
        let (host_dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let host = Arc::new(DirectoryHost::new(host_dir.path()));
        let signer = Arc::new(Ed25519ReleaseSigner::from_seed("bakery.example", [7; 32]).unwrap());
        let release_key: NoteVerifier = signer.verifier_key().parse().unwrap();
        let log = Arc::new(
            SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), "bakery.example", signer)
                .await
                .unwrap(),
        );
        let partitions = SitePartitions::new(&site);
        let table = Partitions::in_memory();
        partitions.install(&table, AuditActor::default()).await.unwrap();
        let facts = Arc::new(InMemoryFactStore::new());
        let charters = Arc::new(InMemoryCharterStore::new());
        let autonomy = Arc::new(InMemoryAutonomyStore::new());
        let ladder = AutonomyLadder::new(autonomy.clone()).with_charters(charters.clone());
        let runtime = SiteRuntime::builder(
            site.clone(),
            base,
            store.clone(),
            Arc::new(BuiltinGenerator),
            host.clone(),
            log,
            work.path(),
        )
        .history(Arc::new(InMemoryContentHistory::default()))
        .charters(charters.clone())
        .facts(facts.clone(), partitions.fact_scope(OWNER));
        let runtime = configure(runtime).build();
        let charter = charter(&site, publish_chain);
        if with_charter {
            let decision = CommitDecision::new("dec-charter-1", chrono::Utc::now()).with_approval(
                RoleApproval::new(Role::Owner, SubjectRef::human(OWNER), chrono::Utc::now()),
            );
            charters.commit(charter.clone(), decision).await.unwrap();
            ladder.sync(&charter).await.unwrap();
        }
        let compiler = LadderCompiler::new(site.clone()).with_charter(&charter, jobs());
        Self {
            site,
            runtime,
            partitions,
            table,
            facts,
            charters,
            autonomy,
            ladder,
            compiler,
            signals: Arc::new(InMemorySignalStore::new()),
            agenda: Arc::new(InMemoryAgendaStore::new()),
            rules: Arc::new(InMemoryStandingRuleStore::new()),
            store,
            search: Arc::new(SiteSearch::new()),
            sessions: Arc::new(InMemorySessionService::new()),
            host,
            release_key,
            dirs: vec![host_dir, work],
        }
    }

    /// A `Known` fact of the owner, as the owner confirmed it.
    pub async fn known_fact(&self, text: &str) -> String {
        self.fact(text, FactStatus::Known, FactSource::UserConfirmed).await
    }

    /// An `Assumed` fact discovery found.
    pub async fn assumed_fact(&self, text: &str) -> String {
        self.fact(text, FactStatus::Assumed, FactSource::ToolObserved).await
    }

    async fn fact(&self, text: &str, status: FactStatus, source: FactSource) -> String {
        let fact = MemoryFact::new(
            self.partitions.fact_scope(OWNER),
            FactKind::ProjectFact,
            text,
            status,
            Provenance::new(source, "seed", "seed"),
        );
        self.facts.put(fact).await.unwrap().id
    }

    pub fn back_office_kit(&self) -> BackOfficeKit {
        BackOfficeKit::new(
            self.partitions.clone(),
            self.table.clone(),
            self.runtime.clone(),
            self.compiler.clone(),
            self.autonomy.clone(),
        )
    }

    pub fn public_kit(&self) -> PublicKit {
        PublicKit {
            partitions: self.partitions.clone(),
            table: self.table.clone(),
            search: self.search.clone(),
            signals: self.partitions.signal_sink(self.signals.clone(), self.agenda.clone()),
            reply: None,
            capabilities: Vec::new(),
        }
    }

    /// A back-office agent `name` over `script`, with `configure` applied last.
    pub fn back_office(
        &mut self,
        name: &str,
        script: Vec<ScriptedResponse>,
        configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
    ) -> (Harness, Arc<ScriptedLlm>) {
        let (kit, rules) = (self.back_office_kit(), self.rules.clone());
        self.harness(name, script, |b| configure(b.website_back_office(&kit).standing_rules(rules)))
    }

    /// A public agent `name` over `script`, with `configure` applied last.
    pub fn public(
        &mut self,
        name: &str,
        script: Vec<ScriptedResponse>,
        configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
    ) -> (Harness, Arc<ScriptedLlm>) {
        let kit = self.public_kit();
        self.harness(name, script, |b| configure(b.website_public(kit)))
    }

    fn harness(
        &mut self,
        name: &str,
        script: Vec<ScriptedResponse>,
        configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
    ) -> (Harness, Arc<ScriptedLlm>) {
        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(ScriptedLlm::new("scripted", script));
        let builder = HarnessAgent::builder(name)
            .model(model.clone())
            .workspace(adk_devtools::Workspace::new(dir.path()))
            .project_instructions(no_user_file())
            .without_compaction();
        let harness = configure(builder)
            .build()
            .unwrap()
            .into_harness(&self.site.name, self.sessions.clone())
            .unwrap();
        self.dirs.push(dir);
        (harness, model)
    }
}

/// Every text the model was sent across `model`'s requests.
pub fn request_text(model: &ScriptedLlm) -> String {
    model
        .requests()
        .iter()
        .flat_map(|request| request.contents.iter())
        .flat_map(|content| content.parts.iter())
        .map(|part| match part {
            adk_core::Part::Text { text } => text.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The loopback site (feature `web-fixture`).
#[cfg(feature = "web-fixture")]
pub mod server {
    use std::sync::Arc;

    use adk_site::host::DirectoryHost;

    /// What the loopback site served and whom it verified.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Hit {
        pub path: String,
        /// The verified Web Bot Auth requester, `None` for an unsigned request.
        pub requester: Option<String>,
    }

    #[derive(Clone)]
    struct ServerState {
        host: Option<Arc<DirectoryHost>>,
        verifier: Option<Arc<adk_auth::web_bot_auth::HttpSignatureVerifier>>,
        hits: Arc<std::sync::Mutex<Vec<Hit>>>,
    }

    /// A loopback web server standing in for the open web and for the bakery's own origin: it
    /// verifies Web Bot Auth signatures against the site's published key directory, redirects
    /// `/redirect` to `/page`, serves an existing bakery page at `/page`, and serves what the
    /// `DirectoryHost` holds. Aborted on drop.
    pub struct SiteServer {
        pub addr: std::net::SocketAddr,
        pub hits: Arc<std::sync::Mutex<Vec<Hit>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for SiteServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn serve(
        axum::extract::State(state): axum::extract::State<ServerState>,
        request: axum::extract::Request,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        // The head alone: a request body is not `Sync`, so it cannot be held across the await.
        let (parts, _body) = request.into_parts();
        let head = axum::http::Request::from_parts(parts, ());
        let requester = match &state.verifier {
            Some(verifier) => verifier.verify_request(&head).await.ok().flatten().map(|r| r.id),
            None => None,
        };
        let path = head.uri().path().to_string();
        state.hits.lock().unwrap().push(Hit { path: path.clone(), requester });
        match path.as_str() {
            "/redirect" => (axum::http::StatusCode::FOUND, [("location", "/page")]).into_response(),
            "/page" => (
                [("content-type", "text/html")],
                "<html><body><h1>Juniper Bakery</h1><p>Open from 7am, Monday to Friday. Rye and sourdough.</p></body></html>",
            )
                .into_response(),
            _ => {
                use adk_site::host::SiteHost;
                match &state.host {
                    Some(host) => match host.fetch(&path).await {
                        Ok(Some(bytes)) => bytes.to_vec().into_response(),
                        _ => axum::http::StatusCode::NOT_FOUND.into_response(),
                    },
                    None => axum::http::StatusCode::NOT_FOUND.into_response(),
                }
            }
        }
    }

    impl SiteServer {
        /// Starts on a loopback port, verifying against `signing`'s directory when given.
        pub async fn start(
            host: Option<Arc<DirectoryHost>>,
            signing: Option<&adk_harness::website::SiteSigning>,
        ) -> Self {
            adk_harness::web::fixture::permit_loopback();
            let verifier = match signing {
                Some(signing) => {
                    let directories =
                        Arc::new(adk_auth::web_bot_auth::InMemoryKeyDirectorySource::new());
                    let directory = adk_auth::web_bot_auth::KeyDirectory::parse(
                        signing.directory_json().as_bytes(),
                        16,
                    )
                    .unwrap();
                    directories.insert(signing.origin(), directory, None).await;
                    Some(Arc::new(
                        adk_auth::web_bot_auth::HttpSignatureVerifier::builder(directories).build(),
                    ))
                }
                None => None,
            };
            let hits = Arc::new(std::sync::Mutex::new(Vec::new()));
            let state = ServerState { host, verifier, hits: hits.clone() };
            let app = axum::Router::new().fallback(serve).with_state(state);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            Self { addr, hits, task }
        }

        pub fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.addr.port())
        }

        pub fn hits(&self) -> Vec<Hit> {
            self.hits.lock().unwrap().clone()
        }
    }
}
