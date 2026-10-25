//! A site of any kind on local fixtures (spec 3.5 §11): its content on a `DirectoryHost`, its
//! two partitions in a kernel partition table, in-memory facts, charters, autonomy records,
//! signals and standing rules, and harness agents over scripted models.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_auth::signed_audit::NoteVerifier;
use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditLogStore, InMemoryAuditRecorder};
use adk_core::principal::SubjectRef;
use adk_core::{Event, Part, UserId};
use adk_harness::website::{
    BackOfficeKit, LadderCompiler, PublicKit, SitePartitions, SiteRef, SiteRuntime, SiteSearch,
};
use adk_harness::{
    ApprovalPrompt, ApprovalPrompter, Harness, HarnessAgent, HarnessAgentBuilder,
    InMemoryFactStore, InMemoryStandingRuleStore, InstructionsConfig, PromptAnswer,
};
use adk_kernel::InMemoryAgendaStore;
use adk_kernel::Partitions;
use adk_kernel::channels::{ChannelSink, DecisionCard, StopNotice};
use adk_kernel::charter::{
    AutonomyLadder, CharterStore, CharterVersion, CommitDecision, InMemoryAutonomyStore,
    InMemoryCharterStore, InMemorySignalStore, RoleApproval,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_session::{InMemorySessionService, SessionService};
use adk_site::content::{InMemoryContentHistory, InMemoryContentStore, SiteConfig, SiteProject};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::DirectoryHost;
use adk_site::log::SiteLog;
use adk_site::signer::{Ed25519ReleaseSigner, ReleaseSigner};
use adk_website::kinds::{KindTemplate, template};
use adk_website::team::SiteTeam;
use adk_website::themes::{ThemeId, theme};
use awp_types::{Charter, LanguageTag, Role, SiteKind};
use futures::StreamExt;
use serde_json::Value;

pub fn user(id: &str) -> UserId {
    UserId::new(id).unwrap()
}

pub fn no_user_file() -> InstructionsConfig {
    InstructionsConfig { user_file: None, require_trust: false, ..InstructionsConfig::default() }
}

/// Collects a run's events, failing on a stream error.
pub async fn collect(stream: adk_core::EventStream) -> Vec<Event> {
    let results = tokio::time::timeout(Duration::from_secs(120), stream.collect::<Vec<_>>())
        .await
        .expect("the run must finish");
    results.into_iter().map(|result| result.expect("event")).collect()
}

/// Every function response in `events`, as `(tool, response)`.
pub fn responses(events: &[Event]) -> Vec<(String, Value)> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some((function_response.name.clone(), function_response.response.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The response to the `nth` call of `tool`.
pub fn response(events: &[Event], tool: &str, nth: usize) -> Value {
    responses(events)
        .into_iter()
        .filter(|(name, _)| name == tool)
        .nth(nth)
        .map(|(_, response)| response)
        .unwrap_or_else(|| panic!("no response {nth} of {tool} in {:#?}", responses(events)))
}

pub fn call(tool: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(tool, args)
}

type Answer = Box<dyn Fn(&ApprovalPrompt) -> PromptAnswer + Send + Sync>;

/// Answers prompts from a queue and records every prompt; an empty queue denies.
#[derive(Default)]
pub struct ScriptedPrompter {
    answers: Mutex<VecDeque<Answer>>,
    pub prompts: Mutex<Vec<ApprovalPrompt>>,
}

impl std::fmt::Debug for ScriptedPrompter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScriptedPrompter")
    }
}

impl ScriptedPrompter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn approve_once(&self) {
        self.answers.lock().unwrap().push_back(Box::new(|_| PromptAnswer::ApproveOnce));
    }

    pub fn held(&self) -> Vec<String> {
        self.prompts.lock().unwrap().iter().map(|p| p.request.tool_name.clone()).collect()
    }
}

#[async_trait::async_trait]
impl ApprovalPrompter for ScriptedPrompter {
    async fn prompt(&self, prompt: ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
        let answer = self.answers.lock().unwrap().pop_front();
        let decided = match &answer {
            Some(answer) => answer(&prompt),
            None => PromptAnswer::Deny { message: Some("no scripted answer".into()) },
        };
        self.prompts.lock().unwrap().push(prompt);
        Ok(decided)
    }
}

/// A channel that keeps what it was sent: the owner's phone.
#[derive(Debug, Default, Clone)]
pub struct Phone {
    pub channel: String,
    pub sent: Arc<Mutex<Vec<(String, String)>>>,
}

impl Phone {
    pub fn new(channel: &str) -> Arc<Self> {
        Arc::new(Self { channel: channel.into(), sent: Arc::default() })
    }

    pub fn messages(&self) -> Vec<String> {
        self.sent.lock().unwrap().iter().map(|(_, text)| text.clone()).collect()
    }
}

#[async_trait::async_trait]
impl ChannelSink for Phone {
    fn channel(&self) -> &str {
        &self.channel
    }

    async fn send_message(&self, thread: &str, text: &str) -> adk_kernel::Result<()> {
        self.sent.lock().unwrap().push((thread.to_string(), text.to_string()));
        Ok(())
    }

    async fn send_decision(&self, thread: &str, card: &DecisionCard) -> adk_kernel::Result<()> {
        self.send_message(thread, &card.text()).await
    }

    async fn send_stopped(&self, thread: &str, notice: &StopNotice) -> adk_kernel::Result<()> {
        self.send_message(thread, &notice.text()).await
    }
}

/// The owner of every fixture site.
pub const OWNER: &str = "amina";

/// A site of one kind on local fixtures.
pub struct Site {
    pub site: SiteRef,
    pub template: KindTemplate,
    pub team: SiteTeam,
    pub base: SiteProject,
    pub runtime: Arc<SiteRuntime>,
    pub partitions: SitePartitions,
    pub table: Partitions,
    pub facts: Arc<InMemoryFactStore>,
    pub charters: Arc<InMemoryCharterStore>,
    pub autonomy: Arc<InMemoryAutonomyStore>,
    pub ladder: AutonomyLadder,
    pub signals: Arc<InMemorySignalStore>,
    pub agenda: Arc<InMemoryAgendaStore>,
    pub rules: Arc<InMemoryStandingRuleStore>,
    pub store: Arc<InMemoryContentStore>,
    pub history: Arc<InMemoryContentHistory>,
    pub search: Arc<SiteSearch>,
    pub sessions: Arc<dyn SessionService>,
    pub host: Arc<DirectoryHost>,
    pub log: Arc<SiteLog>,
    pub release_key: NoteVerifier,
    pub audit: InMemoryAuditRecorder,
    pub dirs: Vec<tempfile::TempDir>,
}

impl Site {
    /// A fresh site of `kind` named `name` at `domain`, in `languages`, with nothing
    /// published.
    pub async fn new(kind: SiteKind, name: &str, domain: &str, languages: &[&str]) -> Self {
        let site = SiteRef::new(
            uuid::Uuid::new_v4(),
            domain.split('.').next().unwrap(),
            format!("https://{domain}"),
        );
        let template = template(kind);
        let team = SiteTeam::new(site.clone(), &template);
        let languages: Vec<LanguageTag> = languages.iter().map(|l| l.parse().unwrap()).collect();
        let mut config = SiteConfig::new(name, domain, languages);
        config.jurisdictions = vec!["KE".into()];
        config.security.contact = vec![format!("mailto:security@{domain}")];
        template.configure_site(&mut config);
        let base = SiteProject::new(config, Vec::new(), chrono::Utc::now());
        let (host_dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let host = Arc::new(DirectoryHost::new(host_dir.path()));
        let signer = Arc::new(Ed25519ReleaseSigner::from_seed(domain, [7; 32]).unwrap());
        let release_key: NoteVerifier = signer.verifier_key().parse().unwrap();
        let log = Arc::new(
            SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), domain, signer).await.unwrap(),
        );
        let partitions = SitePartitions::new(&site);
        let table = Partitions::in_memory();
        partitions.install(&table, AuditActor::default()).await.unwrap();
        let charters = Arc::new(InMemoryCharterStore::new());
        let autonomy = Arc::new(InMemoryAutonomyStore::new());
        let ladder = AutonomyLadder::new(autonomy.clone()).with_charters(charters.clone());
        let facts = Arc::new(InMemoryFactStore::new());
        let store = Arc::new(InMemoryContentStore::default());
        let history = Arc::new(InMemoryContentHistory::default());
        let audit = InMemoryAuditRecorder::new();
        let runtime = assemble_runtime(
            &site,
            &template,
            &base,
            None,
            &store,
            &history,
            &host,
            &log,
            work.path(),
            &charters,
            &facts,
            &partitions,
            &audit,
        );
        Self {
            runtime,
            site,
            template,
            team,
            base,
            partitions,
            table,
            facts,
            charters,
            autonomy,
            ladder,
            signals: Arc::new(InMemorySignalStore::new()),
            agenda: Arc::new(InMemoryAgendaStore::new()),
            rules: Arc::new(InMemoryStandingRuleStore::new()),
            store,
            history,
            search: Arc::new(SiteSearch::new()),
            sessions: Arc::new(InMemorySessionService::new()),
            host,
            log,
            release_key,
            audit,
            dirs: vec![host_dir, work],
        }
    }

    /// A site runtime over the fixture's stores, with `theme` when one is chosen: the build
    /// stage assembles the runtime with the owner's design (spec 3.5 §7.18 stage 6).
    pub fn assemble(&self, chosen: Option<ThemeId>) -> Arc<SiteRuntime> {
        assemble_runtime(
            &self.site,
            &self.template,
            &self.base,
            chosen,
            &self.store,
            &self.history,
            &self.host,
            &self.log,
            self.dirs[1].path(),
            &self.charters,
            &self.facts,
            &self.partitions,
            &self.audit,
        )
    }

    /// Commits `charter` with the approvals of `signers`, and brings its jobs onto the ladder.
    pub async fn commit(&self, charter: &Charter, signers: &[(Role, &str)]) -> CharterVersion {
        let mut decision =
            CommitDecision::new(format!("dec-{}", charter.version), chrono::Utc::now());
        for (role, who) in signers {
            decision = decision.with_approval(RoleApproval::new(
                *role,
                SubjectRef::human(*who),
                chrono::Utc::now(),
            ));
        }
        let version = self.charters.commit(charter.clone(), decision).await.unwrap();
        adk_website::jobs::register(&self.ladder, charter).await.unwrap();
        version
    }

    /// The ladder compiler of the kind's jobs under `charter`.
    pub fn compiler(&self, charter: &Charter) -> LadderCompiler {
        let jobs = self.template.jobs();
        LadderCompiler::new(self.site.clone())
            .with_charter(charter, adk_website::jobs::bindings(&self.team, &jobs))
    }

    pub fn back_office_kit(&self, charter: &Charter) -> BackOfficeKit {
        BackOfficeKit::new(
            self.partitions.clone(),
            self.table.clone(),
            self.runtime.clone(),
            self.compiler(charter),
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

    /// A back-office agent `name` over `script` under `charter`, with `configure` applied last.
    pub fn back_office(
        &mut self,
        name: &str,
        charter: &Charter,
        script: Vec<ScriptedResponse>,
        configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
    ) -> (Harness, Arc<ScriptedLlm>) {
        let kit = self.back_office_kit(charter);
        let rules = self.rules.clone();
        self.harness(name, script, |b| configure(b.website_back_office(&kit).standing_rules(rules)))
    }

    pub fn harness(
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

    /// The audit actions recorded, in order.
    pub fn actions(&self) -> Vec<String> {
        self.audit.records().into_iter().map(|record| record.action).collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn assemble_runtime(
    site: &SiteRef,
    template: &KindTemplate,
    base: &SiteProject,
    chosen: Option<ThemeId>,
    store: &Arc<InMemoryContentStore>,
    history: &Arc<InMemoryContentHistory>,
    host: &Arc<DirectoryHost>,
    log: &Arc<SiteLog>,
    work: &std::path::Path,
    charters: &Arc<InMemoryCharterStore>,
    facts: &Arc<InMemoryFactStore>,
    partitions: &SitePartitions,
    audit: &InMemoryAuditRecorder,
) -> Arc<SiteRuntime> {
    let mut base = base.clone();
    if let Some(id) = chosen {
        base.theme = theme(id);
    }
    SiteRuntime::builder(
        site.clone(),
        base,
        store.clone(),
        Arc::new(BuiltinGenerator),
        host.clone(),
        log.clone(),
        work,
    )
    .history(history.clone())
    .charters(charters.clone())
    .facts(facts.clone(), partitions.fact_scope(OWNER))
    .gate(template.gate(&site.origin))
    .audit(AuditTrail::new(Arc::new(audit.clone())))
    .build()
}

/// A polite source context whose every request is signed with a fresh Web Bot Auth key.
pub fn source_context(domain: &str) -> adk_site::migrate::SourceContext {
    use adk_auth::web_bot_auth::{Ed25519SigningKey, HttpSignatureSigner};
    let key = Arc::new(Ed25519SigningKey::generate().unwrap());
    let signer = HttpSignatureSigner::builder(key, format!("https://{domain}")).build().unwrap();
    let http =
        adk_site::migrate::http::HttpClient::builder().signer(Arc::new(signer)).build().unwrap();
    let acting =
        adk_core::ActingContext::derived(domain, "steward", OWNER, adk_core::SubjectKind::Human);
    adk_site::migrate::SourceContext::new(acting, Arc::new(http))
}

/// Verifies the release the host serves from its published files alone: the checkpoint
/// against the release key, the release's inclusion in the log, and every file's digest
/// against the manifest. Returns the release's sequence number.
pub async fn verify_served(host: &DirectoryHost, domain: &str, key: &NoteVerifier) -> u64 {
    use adk_site::host::{HostTiles, SiteHost};
    use adk_site::log::{CHECKPOINT_PATH, TileProofs, verify_checkpoint, verify_release_inclusion};
    use adk_site::release::{file_digest, manifest_path};
    let note = host.fetch(CHECKPOINT_PATH).await.unwrap().expect("a checkpoint is served");
    let checkpoint =
        verify_checkpoint(&String::from_utf8_lossy(&note), domain, std::slice::from_ref(key))
            .unwrap();
    let seq = checkpoint.size;
    let manifest_bytes =
        host.fetch(&manifest_path(seq)).await.unwrap().expect("the manifest is served");
    let tiles = HostTiles(host);
    let proofs = TileProofs::new(&tiles, seq);
    let entry = proofs.entry(seq - 1).await.unwrap();
    let proof = proofs.inclusion_proof(seq - 1).await.unwrap();
    verify_release_inclusion(&checkpoint, &entry, &proof, seq, &file_digest(&manifest_bytes))
        .unwrap();
    let manifest: awp_types::ReleaseManifest = serde_json::from_slice(&manifest_bytes).unwrap();
    for path in manifest.files.keys().filter(|path| path.ends_with(".html")) {
        let served = host.fetch(path).await.unwrap().unwrap_or_else(|| panic!("{path} is served"));
        assert_eq!(file_digest(&served), manifest.files[path].sha256, "{path} matches its release");
    }
    seq
}
