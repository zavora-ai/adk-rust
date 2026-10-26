//! A site of any kind on local fixtures (spec 3.5 §11 "Fixtures"): its content on a
//! `DirectoryHost`, its two partitions in a kernel partition table, in-memory facts, charters,
//! autonomy records, signals and standing rules, and harness agents over the run's model.

pub mod edge;
pub mod legacy;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_auth::signed_audit::NoteVerifier;
use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditLogStore, InMemoryAuditRecorder};
use adk_core::principal::SubjectRef;
use adk_core::{Clock, Event, Llm, Part, UserId};
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
use adk_site::gate::{LaunchGate, Quarantine};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::DirectoryHost;
use adk_site::log::SiteLog;
use adk_site::pipeline::{Change, EntryPoint, PipelineOutcome};
use adk_site::signer::{Ed25519ReleaseSigner, ReleaseSigner};
use adk_website::edge::adapters::DeferredAnnouncer;
use adk_website::kinds::{CharterInputs, KindTemplate, template};
use adk_website::onboarding::build::SitePlan;
use adk_website::team::{AgentRole, BuiltTeam, SiteTeam, TeamKit};
use adk_website::themes::{ThemeId, theme};
use anyhow::{Context, Result, anyhow};
use awp_types::{Channel, Charter, ContentItem, LanguageTag, Role, SiteKind};
use futures::StreamExt;
use serde_json::Value;

use crate::live::Mode;

/// The owner of every fixture site.
pub const OWNER: &str = "amina";

/// The release key's seed every fixture site signs with.
pub const RELEASE_SEED: [u8; 32] = [7; 32];

/// `id` as a user id.
pub fn user(id: &str) -> UserId {
    UserId::new(id).expect("a valid user id")
}

/// Instructions without a user file, so a run reads nothing from the machine it runs on.
pub fn no_user_file() -> InstructionsConfig {
    InstructionsConfig { user_file: None, require_trust: false, ..InstructionsConfig::default() }
}

/// Collects a run's events.
///
/// # Errors
///
/// A stream error, or a run that does not finish within five minutes.
pub async fn collect(stream: adk_core::EventStream) -> Result<Vec<Event>> {
    let results = tokio::time::timeout(Duration::from_secs(300), stream.collect::<Vec<_>>())
        .await
        .context("the run did not finish")?;
    results.into_iter().map(|result| result.map_err(|e| anyhow!("{e}"))).collect()
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

/// The response to the `nth` call of `tool`, if there was one.
pub fn response(events: &[Event], tool: &str, nth: usize) -> Option<Value> {
    responses(events).into_iter().filter(|(name, _)| name == tool).nth(nth).map(|(_, r)| r)
}

/// Prints a live run's calls, results and text, so the report shows what the model did.
pub fn trace(events: &[Event]) {
    for event in events.iter().filter(|e| !e.llm_response.partial) {
        for part in event.llm_response.content.iter().flat_map(|c| c.parts.iter()) {
            match part {
                Part::FunctionCall { name, args, .. } => println!("      call {name} {args}"),
                Part::FunctionResponse { function_response, .. } => {
                    let shown = function_response.response.to_string();
                    let shown: String = shown.chars().take(240).collect();
                    println!("      result {} {shown}", function_response.name);
                }
                Part::Text { text } => {
                    let shown: String = text.chars().take(400).collect();
                    println!("      says: {shown}");
                }
                _ => {}
            }
        }
    }
}

/// A scripted call of `tool` with `args`.
pub fn call(tool: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(tool, args)
}

type Answer = Box<dyn Fn(&ApprovalPrompt) -> PromptAnswer + Send + Sync>;

/// Answers prompts from a queue and records every prompt; an empty queue denies. The owner's
/// side of a held call.
#[derive(Default)]
pub struct ScriptedPrompter {
    answers: Mutex<VecDeque<Answer>>,
    /// Every prompt, in order.
    pub prompts: Mutex<Vec<ApprovalPrompt>>,
}

impl std::fmt::Debug for ScriptedPrompter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScriptedPrompter")
    }
}

impl ScriptedPrompter {
    /// A prompter that denies until told otherwise.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The next prompt is approved once.
    pub fn approve_once(&self) {
        self.answers.lock().unwrap().push_back(Box::new(|_| PromptAnswer::ApproveOnce));
    }

    /// The held tools, in order.
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
    /// The channel's name.
    pub channel: String,
    /// `(thread, text)` of every message.
    pub sent: Arc<Mutex<Vec<(String, String)>>>,
}

impl Phone {
    /// A phone on `channel`.
    pub fn new(channel: &str) -> Arc<Self> {
        Arc::new(Self { channel: channel.into(), sent: Arc::default() })
    }

    /// Every message's text.
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

/// A site of one kind on local fixtures.
pub struct Site {
    /// The site.
    pub site: SiteRef,
    /// Its kind's template.
    pub template: KindTemplate,
    /// Its team.
    pub team: SiteTeam,
    /// The project the runtime builds from.
    pub base: SiteProject,
    /// The site runtime: pipeline, gate, host, log.
    pub runtime: Arc<SiteRuntime>,
    /// The public and back-office partitions.
    pub partitions: SitePartitions,
    /// The kernel's partition table.
    pub table: Partitions,
    /// The owner's long-term facts.
    pub facts: Arc<InMemoryFactStore>,
    /// The charter store.
    pub charters: Arc<InMemoryCharterStore>,
    /// Autonomy records.
    pub autonomy: Arc<InMemoryAutonomyStore>,
    /// The autonomy ladder.
    pub ladder: AutonomyLadder,
    /// Signals into the back office.
    pub signals: Arc<InMemorySignalStore>,
    /// The kernel's agenda.
    pub agenda: Arc<InMemoryAgendaStore>,
    /// Standing rules the owner created.
    pub rules: Arc<InMemoryStandingRuleStore>,
    /// The content repository.
    pub store: Arc<InMemoryContentStore>,
    /// Its history.
    pub history: Arc<InMemoryContentHistory>,
    /// The release's search index.
    pub search: Arc<SiteSearch>,
    /// Agent sessions.
    pub sessions: Arc<dyn SessionService>,
    /// The static host.
    pub host: Arc<DirectoryHost>,
    /// The site log.
    pub log: Arc<SiteLog>,
    /// The key releases verify against.
    pub release_key: NoteVerifier,
    /// Every audit record.
    pub audit: InMemoryAuditRecorder,
    /// Every runtime announces through it; an edge sets its target.
    pub announcer: Arc<DeferredAnnouncer>,
    /// The runtime's clock.
    pub clock: Option<Arc<dyn Clock>>,
    /// The launch gate the runtime runs: the kind's, plus any extension a scenario adds.
    pub gate: LaunchGate,
    /// Subjects a migration holds back.
    pub quarantine: Option<Quarantine>,
    /// The generator the runtime builds with: the built-in renderer unless a scenario wraps it.
    pub generator: Arc<dyn adk_site::generator::SiteGenerator>,
    /// The host the runtime publishes to instead of `host`, such as an object-store host.
    pub serve_on: Option<Arc<dyn adk_site::host::SiteHost>>,
    /// The temporary directories: the host first, the work directory second.
    pub dirs: Vec<tempfile::TempDir>,
}

impl Site {
    /// A fresh site of `kind` named `name` at `domain`, in `languages`, with nothing published.
    ///
    /// # Errors
    ///
    /// When a store, the log or the partitions cannot be set up.
    pub async fn new(kind: SiteKind, name: &str, domain: &str, languages: &[&str]) -> Result<Self> {
        Self::with(kind, name, domain, languages, None, |_| {}).await
    }

    /// A fresh site with the runtime on `clock` and `configure` applied to its configuration.
    ///
    /// # Errors
    ///
    /// When a store, the log or the partitions cannot be set up.
    pub async fn with(
        kind: SiteKind,
        name: &str,
        domain: &str,
        languages: &[&str],
        clock: Option<Arc<dyn Clock>>,
        configure: impl FnOnce(&mut SiteConfig),
    ) -> Result<Self> {
        let site = SiteRef::new(
            uuid::Uuid::new_v4(),
            domain.split('.').next().unwrap_or(domain),
            format!("https://{domain}"),
        );
        let template = template(kind);
        let team = SiteTeam::new(site.clone(), &template);
        let languages: Vec<LanguageTag> =
            languages.iter().map(|l| l.parse()).collect::<std::result::Result<_, _>>()?;
        let mut config = SiteConfig::new(name, domain, languages);
        config.jurisdictions = vec!["KE".into()];
        config.security.contact = vec![format!("mailto:security@{domain}")];
        template.configure_site(&mut config);
        configure(&mut config);
        let base = SiteProject::new(config, Vec::new(), chrono::Utc::now());
        let (host_dir, work) = (tempfile::tempdir()?, tempfile::tempdir()?);
        let host = Arc::new(DirectoryHost::new(host_dir.path()));
        let signer = Arc::new(Ed25519ReleaseSigner::from_seed(domain, RELEASE_SEED)?);
        let release_key: NoteVerifier =
            signer.verifier_key().parse().map_err(|e| anyhow!("release key: {e:?}"))?;
        let log =
            Arc::new(SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), domain, signer).await?);
        let partitions = SitePartitions::new(&site);
        let table = Partitions::in_memory();
        partitions.install(&table, AuditActor::default()).await?;
        let charters = Arc::new(InMemoryCharterStore::new());
        let autonomy = Arc::new(InMemoryAutonomyStore::new());
        let ladder = AutonomyLadder::new(autonomy.clone()).with_charters(charters.clone());
        let fixed = Fixed {
            site: &site,
            base: &base,
            store: Arc::new(InMemoryContentStore::default()),
            history: Arc::new(InMemoryContentHistory::default()),
            host: &host,
            log: &log,
            charters: &charters,
            facts: Arc::new(InMemoryFactStore::new()),
            partitions: &partitions,
            audit: InMemoryAuditRecorder::new(),
            announcer: Arc::new(DeferredAnnouncer::default()),
            clock: clock.clone(),
            gate: template.gate(&site.origin),
            quarantine: None,
            generator: Arc::new(BuiltinGenerator),
            serve_on: None,
        };
        let runtime = fixed.runtime(None, work.path());
        let Fixed { store, history, facts, audit, announcer, gate, generator, .. } = fixed;
        Ok(Self {
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
            announcer,
            clock,
            gate,
            quarantine: None,
            generator,
            serve_on: None,
            dirs: vec![host_dir, work],
        })
    }

    /// The host's directory.
    pub fn host_dir(&self) -> &std::path::Path {
        self.dirs[0].path()
    }

    /// A site runtime over the fixture's stores, with `chosen` as the theme when one is
    /// chosen: the build stage assembles the runtime with the owner's design (spec 3.5 §7.18
    /// stage 6).
    pub fn assemble(&self, chosen: Option<ThemeId>) -> Arc<SiteRuntime> {
        Fixed {
            site: &self.site,
            base: &self.base,
            store: self.store.clone(),
            history: self.history.clone(),
            host: &self.host,
            log: &self.log,
            charters: &self.charters,
            facts: self.facts.clone(),
            partitions: &self.partitions,
            audit: self.audit.clone(),
            announcer: self.announcer.clone(),
            clock: self.clock.clone(),
            gate: self.gate.clone(),
            quarantine: self.quarantine.clone(),
            generator: self.generator.clone(),
            serve_on: self.serve_on.clone(),
        }
        .runtime(chosen, self.dirs[1].path())
    }

    /// The charter of this site from `inputs` built by `adjust`, drafted from the kind's
    /// template with SMS for P1 alarms.
    pub fn charter(&self, name: &str, mission: &str, currency: &str) -> Charter {
        let languages = self.base.config.languages.clone();
        let mut inputs =
            CharterInputs::new(self.site.id, name, OWNER, mission, languages, currency);
        inputs.p1_channels = vec![Channel::Sms];
        inputs.p2_channels = vec![Channel::Whatsapp];
        self.template.draft_charter(&self.team, &inputs)
    }

    /// Commits `charter` with the approvals of `signers`, and brings its jobs onto the ladder.
    ///
    /// # Errors
    ///
    /// When the charter store or the ladder refuses.
    pub async fn commit(
        &self,
        charter: &Charter,
        signers: &[(Role, &str)],
    ) -> Result<CharterVersion> {
        let mut decision =
            CommitDecision::new(format!("dec-{}", charter.version), chrono::Utc::now());
        for (role, who) in signers {
            decision = decision.with_approval(RoleApproval::new(
                *role,
                SubjectRef::human(*who),
                chrono::Utc::now(),
            ));
        }
        let version = self.charters.commit(charter.clone(), decision).await?;
        adk_website::jobs::register(&self.ladder, charter).await?;
        Ok(version)
    }

    /// Commits the kind's charter and publishes release 1: the plan's pages and `extra`, by
    /// the Steward under the onboarding job.
    ///
    /// # Errors
    ///
    /// When the commit fails or the release is not published.
    pub async fn launch(
        &self,
        name: &str,
        currency: &str,
        extra: Vec<ContentItem>,
    ) -> Result<Charter> {
        let charter = self.charter(name, "Fresh bread every morning.", currency);
        self.commit(&charter, &[(Role::Owner, OWNER)]).await?;
        let mut items = SitePlan::new(&charter, name).items(&charter, &[], &["KE".to_string()]);
        items.extend(extra);
        let outcome = self.runtime.pipeline().run(self.steward_change(items, "onboarding")).await?;
        match outcome {
            PipelineOutcome::Published(_) => Ok(charter),
            other => Err(anyhow!("release 1 was not published: {other:?}")),
        }
    }

    /// Launches the site as onboarding does: the root signs the contract over the charter's
    /// public terms, the documents join the project, and release 1 publishes the plan's pages
    /// and `extra`. Returns the charter and the site's keys.
    ///
    /// # Errors
    ///
    /// When the documents cannot be signed or release 1 is not published.
    pub async fn launch_signed(
        &mut self,
        name: &str,
        currency: &str,
        extra: Vec<ContentItem>,
    ) -> Result<(Charter, adk_website::contract::SiteKeys)> {
        let charter = self.charter(name, "Fresh bread every morning.", currency);
        self.commit(&charter, &[(Role::Owner, OWNER)]).await?;
        let keys = self.publish_signed(&charter, name, extra).await?;
        Ok((charter, keys))
    }

    /// Release 1 under the committed `charter`: the root signs the contract over its public
    /// terms, the documents join the project, and the plan's pages and `extra` publish.
    ///
    /// # Errors
    ///
    /// When the documents cannot be signed or release 1 is not published.
    pub async fn publish_signed(
        &mut self,
        charter: &Charter,
        name: &str,
        extra: Vec<ContentItem>,
    ) -> Result<adk_website::contract::SiteKeys> {
        use adk_website::contract::{ContractInputs, add_to_project, site_documents};
        let keys = edge::site_keys(&self.base.config.domain)?;
        let inputs = ContractInputs {
            id: self.site.id,
            name: name.into(),
            domain: self.base.config.domain.clone(),
            languages: self.base.config.languages.clone(),
            edge: Some(self.site.origin.clone()),
            release_seq: 1,
            terms: charter.public.clone(),
        };
        let (documents, _) = site_documents(&keys, &inputs, chrono::Utc::now()).await?;
        add_to_project(&mut self.base, documents, charter.public.clone());
        self.runtime = self.assemble(None);
        let mut items = SitePlan::new(charter, name).items(charter, &[], &["KE".to_string()]);
        items.extend(extra);
        match self.runtime.pipeline().run(self.steward_change(items, "onboarding")).await? {
            PipelineOutcome::Published(_) => Ok(keys),
            other => Err(anyhow!("release 1 was not published: {other:?}")),
        }
    }

    /// A change of `items` the Steward proposes under `job`.
    pub fn steward_change(&self, items: Vec<ContentItem>, job: &str) -> Change {
        let steward = AgentRole::Steward.principal(&self.site);
        Change {
            items,
            media: Vec::new(),
            entry: EntryPoint::SiteAgent { agent: steward.clone() },
            proposer: steward,
            job: job.into(),
            action_class: "publish".into(),
        }
    }

    /// The ladder compiler of the kind's jobs under `charter`.
    pub fn compiler(&self, charter: &Charter) -> LadderCompiler {
        let jobs = self.template.jobs();
        LadderCompiler::new(self.site.clone())
            .with_charter(charter, adk_website::jobs::bindings(&self.team, &jobs))
    }

    /// The back office's kit under `charter`.
    pub fn back_office_kit(&self, charter: &Charter) -> BackOfficeKit {
        BackOfficeKit::new(
            self.partitions.clone(),
            self.table.clone(),
            self.runtime.clone(),
            self.compiler(charter),
            self.autonomy.clone(),
        )
    }

    /// The public partition's kit.
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

    /// A back-office agent `name` under `charter`: on the live model in a live run, else on
    /// `script`, with `configure` applied last.
    ///
    /// # Errors
    ///
    /// When the harness refuses its configuration.
    pub fn back_office(
        &mut self,
        mode: &Mode,
        name: &str,
        charter: &Charter,
        script: Vec<ScriptedResponse>,
        configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
    ) -> Result<(Harness, Arc<ScriptedLlm>)> {
        let kit = self.back_office_kit(charter);
        let rules = self.rules.clone();
        self.harness(mode, name, script, |b| {
            configure(b.website_back_office(&kit).standing_rules(rules))
        })
    }

    /// The agent `role` of the site's team as the product builds it — `SiteTeam::build` over
    /// a `TeamKit` with the owner's `wallet` — on the live model in a live run, else on
    /// `script`, with `configure` applied to `role`'s builder last; with the built team.
    ///
    /// # Errors
    ///
    /// When the team does not build or the runner refuses its configuration.
    pub async fn team_member(
        &mut self,
        mode: &Mode,
        charter: &Charter,
        role: AgentRole,
        script: Vec<ScriptedResponse>,
        wallet: Option<adk_payments::wallet::Wallet>,
        configure: impl Fn(HarnessAgentBuilder) -> HarnessAgentBuilder + Send + Sync + 'static,
    ) -> Result<(Harness, Arc<ScriptedLlm>, BuiltTeam)> {
        let dir = tempfile::tempdir()?;
        let scripted = Arc::new(ScriptedLlm::new("scripted", script));
        let model: Arc<dyn Llm> = mode.live_model().unwrap_or_else(|| scripted.clone());
        let public = {
            let (partitions, table, search) =
                (self.partitions.clone(), self.table.clone(), self.search.clone());
            let sink = self.partitions.signal_sink(self.signals.clone(), self.agenda.clone());
            move || PublicKit {
                partitions: partitions.clone(),
                table: table.clone(),
                search: search.clone(),
                signals: sink.clone(),
                reply: None,
                capabilities: Vec::new(),
            }
        };
        let kit = TeamKit {
            model,
            root: dir.path().to_path_buf(),
            back_office: self.back_office_kit(charter),
            public: Arc::new(public),
            standing_rules: self.rules.clone(),
            configure: Arc::new(move |member, builder| {
                let builder = builder
                    .project_instructions(no_user_file())
                    .without_compaction()
                    .max_iterations(16);
                if member == role { configure(builder) } else { builder }
            }),
            wallet,
        };
        let team = self.team.build(&kit).await.map_err(|e| anyhow!("{e}"))?;
        let agent = team.agent(role).cloned().ok_or_else(|| anyhow!("the kind runs no {role}"))?;
        let mut runner = agent.configure_runner(
            adk_runner::Runner::builder()
                .app_name(self.site.name.clone())
                .agent(agent.clone())
                .session_service(self.sessions.clone()),
        );
        // As `HarnessAgent::into_harness` does: the agent acts as its partition's principal.
        if let Some(partition) = agent.partition() {
            runner =
                runner.identity_source(partition.identity_source(&self.site.name, role.as_str()));
        }
        let harness = Harness::with_runner(agent, runner.build().map_err(|e| anyhow!("{e}"))?)
            .map_err(|e| anyhow!("{e}"))?;
        self.dirs.push(dir);
        Ok((harness, scripted, team))
    }

    /// A harness agent `name`: on the live model in a live run, else on `script`.
    ///
    /// # Errors
    ///
    /// When the harness refuses its configuration.
    pub fn harness(
        &mut self,
        mode: &Mode,
        name: &str,
        script: Vec<ScriptedResponse>,
        configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
    ) -> Result<(Harness, Arc<ScriptedLlm>)> {
        let dir = tempfile::tempdir()?;
        let scripted = Arc::new(ScriptedLlm::new("scripted", script));
        let model: Arc<dyn Llm> = mode.live_model().unwrap_or_else(|| scripted.clone());
        let builder = HarnessAgent::builder(name)
            .model(model)
            .workspace(adk_devtools::Workspace::new(dir.path()))
            .project_instructions(no_user_file())
            .without_compaction()
            .max_iterations(16);
        let harness = configure(builder)
            .build()
            .map_err(|e| anyhow!("{e}"))?
            .into_harness(&self.site.name, self.sessions.clone())
            .map_err(|e| anyhow!("{e}"))?;
        self.dirs.push(dir);
        Ok((harness, scripted))
    }

    /// The audit actions recorded, in order.
    pub fn actions(&self) -> Vec<String> {
        self.audit.records().into_iter().map(|record| record.action).collect()
    }
}

/// What a site runtime is assembled from.
struct Fixed<'a> {
    site: &'a SiteRef,
    base: &'a SiteProject,
    store: Arc<InMemoryContentStore>,
    history: Arc<InMemoryContentHistory>,
    host: &'a Arc<DirectoryHost>,
    log: &'a Arc<SiteLog>,
    charters: &'a Arc<InMemoryCharterStore>,
    facts: Arc<InMemoryFactStore>,
    partitions: &'a SitePartitions,
    audit: InMemoryAuditRecorder,
    announcer: Arc<DeferredAnnouncer>,
    clock: Option<Arc<dyn Clock>>,
    gate: LaunchGate,
    quarantine: Option<Quarantine>,
    generator: Arc<dyn adk_site::generator::SiteGenerator>,
    serve_on: Option<Arc<dyn adk_site::host::SiteHost>>,
}

impl Fixed<'_> {
    fn runtime(&self, chosen: Option<ThemeId>, work: &std::path::Path) -> Arc<SiteRuntime> {
        let mut base = self.base.clone();
        if let Some(id) = chosen {
            base.theme = theme(id);
        }
        let mut builder = SiteRuntime::builder(
            self.site.clone(),
            base,
            self.store.clone(),
            self.generator.clone(),
            self.serve_on
                .clone()
                .unwrap_or_else(|| self.host.clone() as Arc<dyn adk_site::host::SiteHost>),
            self.log.clone(),
            work,
        )
        .history(self.history.clone())
        .charters(self.charters.clone())
        .facts(self.facts.clone(), self.partitions.fact_scope(OWNER))
        .gate(self.gate.clone())
        .audit(AuditTrail::new(Arc::new(self.audit.clone())))
        .announcer(self.announcer.clone());
        if let Some(clock) = &self.clock {
            builder = builder.clock(clock.clone());
        }
        if let Some(quarantine) = &self.quarantine {
            builder = builder.quarantine(quarantine.clone());
        }
        builder.build()
    }
}

/// Verifies the release the host serves from its published files alone: the checkpoint
/// against the release key, the release's inclusion in the log, and every page's digest
/// against the manifest. Returns the release's sequence number.
///
/// # Errors
///
/// The first link that does not verify.
pub async fn verify_served(host: &DirectoryHost, domain: &str, key: &NoteVerifier) -> Result<u64> {
    use adk_site::host::{HostTiles, SiteHost};
    use adk_site::log::{CHECKPOINT_PATH, TileProofs, verify_checkpoint, verify_release_inclusion};
    use adk_site::release::{file_digest, manifest_path};
    let note = host.fetch(CHECKPOINT_PATH).await?.context("no checkpoint is served")?;
    let checkpoint =
        verify_checkpoint(&String::from_utf8_lossy(&note), domain, std::slice::from_ref(key))?;
    let seq = checkpoint.size;
    let manifest_bytes =
        host.fetch(&manifest_path(seq)).await?.context("the manifest is not served")?;
    let tiles = HostTiles(host);
    let proofs = TileProofs::new(&tiles, seq);
    let entry = proofs.entry(seq - 1).await?;
    let proof = proofs.inclusion_proof(seq - 1).await?;
    verify_release_inclusion(&checkpoint, &entry, &proof, seq, &file_digest(&manifest_bytes))?;
    let manifest: awp_types::ReleaseManifest = serde_json::from_slice(&manifest_bytes)?;
    for path in manifest.files.keys().filter(|path| path.ends_with(".html")) {
        let served = host.fetch(path).await?.with_context(|| format!("{path} is not served"))?;
        anyhow::ensure!(
            file_digest(&served) == manifest.files[path].sha256,
            "{path} does not match its release"
        );
    }
    Ok(seq)
}

/// A polite source context whose every request is signed with a fresh Web Bot Auth key.
///
/// # Errors
///
/// When the key or the client cannot be made.
pub fn source_context(domain: &str) -> Result<adk_site::migrate::SourceContext> {
    use adk_auth::web_bot_auth::{Ed25519SigningKey, HttpSignatureSigner};
    let key = Arc::new(Ed25519SigningKey::generate()?);
    let signer = HttpSignatureSigner::builder(key, format!("https://{domain}")).build()?;
    let http = adk_site::migrate::http::HttpClient::builder().signer(Arc::new(signer)).build()?;
    let acting =
        adk_core::ActingContext::derived(domain, "steward", OWNER, adk_core::SubjectKind::Human);
    Ok(adk_site::migrate::SourceContext::new(acting, Arc::new(http)))
}
