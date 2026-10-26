//! The product's parts (spec 3.5 §7.16, §7.17, §7.19, §7.20, §7.14): playbooks are skill
//! files the harness can load; a scaffolded project loads and builds; the jobs become kernel
//! processes; the back office's weekly review runs as a kernel workflow; the wallet answers
//! the KPI tracker for its own site only; a business runs all eight agents.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_core::{ManualClock, SubjectRef};
use adk_kernel::charter::{InMemoryKpiStore, KpiSource, KpiTracker, SiteProcessKind};
use adk_kernel::wake::Wake;
use adk_payments::wallet::{
    InMemoryWalletProvider, InMemoryWalletStore, Wallet, WalletKpiSource, WalletPolicy,
};
use adk_server::background::WorkflowExecutor;
use adk_site::content::SiteProject;
use adk_site::generator::{BuiltinGenerator, SiteGenerator};
use adk_website::jobs::{self, DIGEST_WORKFLOW, PROBE_WORKFLOW};
use adk_website::kinds::{CharterInputs, template};
use adk_website::onboarding::scaffold::{ScaffoldOptions, scaffold};
use adk_website::playbooks::PLAYBOOKS;
use adk_website::review::ReviewComposer;
use adk_website::team::{AgentRole, SiteTeam, TeamKit};
use adk_website::themes::ThemeId;
use adk_website::wallet::WalletKpi;
use adk_website::workflows::{DigestTarget, SiteWorkflows};
use awp_types::{Role, SiteKind};
use common::*;

#[test]
fn every_playbook_is_a_skill_the_harness_loads() {
    for playbook in PLAYBOOKS {
        let path = std::path::PathBuf::from(format!(".skills/{}.md", playbook.id));
        let parsed = adk_skill::parse_skill_markdown(&path, playbook.text)
            .unwrap_or_else(|e| panic!("{}: {e}", playbook.id));
        assert_eq!(parsed.name, playbook.id);
        assert!(parsed.tags.contains(&format!("job:{}", playbook.job)), "{}", playbook.id);
        for agent in playbook.agents {
            assert!(parsed.tags.contains(&format!("agent:{agent}")), "{} {agent}", playbook.id);
        }
        assert!(parsed.body.contains("## Steps") && parsed.body.contains("## Limits"));
    }
    let jobs: BTreeSet<&str> = jobs::JOBS.iter().map(|job| job.id).collect();
    let covered: BTreeSet<&str> = PLAYBOOKS.iter().map(|p| p.job).collect();
    assert_eq!(jobs, covered, "a playbook for every job of §7.20");
}

#[tokio::test]
async fn a_scaffolded_project_loads_and_builds_with_its_theme() {
    let dir = tempfile::tempdir().unwrap();
    let options = ScaffoldOptions {
        name: "Kaunti ya Nakuru".into(),
        domain: "nakuru.go.ke".into(),
        kind: SiteKind::Government,
        languages: vec!["en".parse().unwrap(), "sw".parse().unwrap()],
        theme: ThemeId::Harbour,
        currency: "KES".into(),
    };
    let written = scaffold(dir.path(), &options).await.unwrap();
    for file in [
        "site.toml",
        "theme/theme.toml",
        "theme/style.css",
        "charter.draft.json",
        "capabilities.json",
        "public-terms.json",
        "public/index.html",
    ] {
        assert!(written.iter().any(|w| w == file), "{file}");
    }
    assert!(scaffold(dir.path(), &options).await.is_err(), "an existing project is left alone");
    let project = SiteProject::load(dir.path(), chrono::Utc::now()).await.unwrap();
    assert_eq!(project.theme.name, "harbour");
    assert_eq!(project.config.translation.required.len(), 2, "every official language");
    assert!(project.config.site_capabilities.iter().any(|c| c == "submit_service_form"));
    let charter: awp_types::Charter =
        serde_json::from_slice(&std::fs::read(dir.path().join("charter.draft.json")).unwrap())
            .unwrap();
    assert_eq!(charter.kind, SiteKind::Government);
    assert!(charter.signed.is_none(), "a draft until the owner signs");
    let out = tempfile::tempdir().unwrap();
    let report = BuiltinGenerator.build(&project, out.path()).await.unwrap();
    assert_eq!(report.theme, "harbour");
    let css = std::fs::read_dir(out.path()).unwrap().flatten().any(|entry| {
        entry.path().extension().is_some_and(|e| e == "css")
            && std::fs::read_to_string(entry.path()).unwrap().contains("prefers-color-scheme: dark")
    });
    let nested_css = report.outputs.iter().filter(|p| p.ends_with(".css")).any(|p| {
        std::fs::read_to_string(out.path().join(p)).unwrap().contains("prefers-color-scheme: dark")
    });
    assert!(css || nested_css, "the dark scheme ships: {:?}", report.outputs);
}

#[tokio::test]
async fn the_jobs_become_kernel_processes_and_the_digest_runs_as_a_workflow() {
    let site = Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let partition = site.partitions.back_office().id.clone();
    let processes = jobs::processes(&site.team, &site.template.jobs(), &site.site.name, &partition);
    let kinds: BTreeMap<SiteProcessKind, &jobs::SiteProcess> =
        processes.iter().map(|p| (p.kind, p)).collect();
    assert_eq!(kinds.len(), 7, "a business runs every site process");
    assert_eq!(kinds[&SiteProcessKind::Probe].target, PROBE_WORKFLOW);
    assert_eq!(kinds[&SiteProcessKind::Digest].target, DIGEST_WORKFLOW);
    assert_eq!(kinds[&SiteProcessKind::Outreach].target, "revenue");
    assert!(
        matches!(&kinds[&SiteProcessKind::Probe].trigger.wake, Wake::Cron { expr, .. } if expr == "0 */5 * * * *")
    );
    assert!(
        matches!(&kinds[&SiteProcessKind::Content].trigger.wake, Wake::Signal { kind, .. } if kind == "content_gap")
    );
    for process in &processes {
        assert_eq!(process.trigger.partition, partition, "the back office runs them");
        assert_eq!(process.trigger.run.input["playbook"], jobs::job(process.job).unwrap().playbook);
    }

    let inputs = CharterInputs::new(
        site.site.id,
        "Juniper Bakery",
        OWNER,
        "Bread.",
        vec!["en".parse().unwrap()],
        "KES",
    );
    let charter = site.template.draft_charter(&site.team, &inputs);
    site.commit(&charter, &[(Role::Owner, OWNER)]).await;
    let tracker =
        KpiTracker::new(site.site.id, site.charters.clone(), Arc::new(InMemoryKpiStore::new()));
    let composer = Arc::new(ReviewComposer::new(
        site.site.id,
        tracker,
        site.ladder.clone(),
        site.charters.clone(),
    ));
    let phone = Phone::new("telegram");
    let workflows = SiteWorkflows::new().digest(
        site.site.id,
        DigestTarget { composer, sink: phone.clone(), thread: "owner".into() },
    );
    assert!(workflows.has_workflow(DIGEST_WORKFLOW) && workflows.has_workflow(PROBE_WORKFLOW));
    let input = [("site".to_string(), serde_json::json!(site.site.id))].into_iter().collect();
    let output = workflows
        .execute(DIGEST_WORKFLOW, input, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert!(output["document"].as_str().unwrap().starts_with("# Weekly review"));
    assert_eq!(phone.messages().len(), 1, "summarised on the owner's channel");
    let missing =
        [("site".to_string(), serde_json::json!(uuid::Uuid::new_v4()))].into_iter().collect();
    assert!(
        workflows
            .execute(PROBE_WORKFLOW, missing, tokio_util::sync::CancellationToken::new())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_wallet_answers_the_kpi_tracker_for_its_own_site_only() {
    let site = uuid::Uuid::new_v4();
    let clock: Arc<dyn adk_core::Clock> = Arc::new(ManualClock::new(chrono::Utc::now()));
    let wallet = Wallet::builder(
        "bakery",
        WalletPolicy::new("KES"),
        Arc::new(
            InMemoryWalletProvider::stored_value("treasury", "KES", 10_000)
                .with_clock(clock.clone()),
        ),
    )
    .store(Arc::new(InMemoryWalletStore::new()))
    .owner(SubjectRef::human(OWNER))
    .clock(clock)
    .build()
    .unwrap();
    let source = WalletKpi::new(WalletKpiSource::new(wallet, site));
    assert_eq!(source.name(), "spend");
    let now = chrono::Utc::now();
    let spend = source.read(site, "spend", now - chrono::Duration::days(7), now).await.unwrap();
    assert_eq!(spend, Some(0.0));
    assert!(
        source
            .read(uuid::Uuid::new_v4(), "spend", now - chrono::Duration::days(7), now)
            .await
            .is_err(),
        "A-I22"
    );
}

/// The owner's wallet: hosting billed by the host, within a monthly cap.
fn finance_wallet() -> Wallet {
    let policy = WalletPolicy::new("KES").with_mandate(
        adk_payments::wallet::SpendCategory::Hosting,
        adk_payments::wallet::Mandate::new(
            50_000,
            20_000,
            adk_payments::wallet::RailRef::provider_billing("hostco"),
        )
        .with_payee(adk_payments::wallet::PayeeRule::Id("hostco".into())),
    );
    Wallet::builder(
        "juniper-example",
        policy,
        Arc::new(InMemoryWalletProvider::stored_value("treasury", "KES", 100_000)),
    )
    .rail(Arc::new(adk_payments::wallet::ProviderBillingRail::new(Arc::new(
        adk_payments::wallet::InMemoryBillingAccount::new("hostco", "KES"),
    ))))
    .owner(SubjectRef::human(OWNER))
    .build()
    .unwrap()
}

#[tokio::test]
async fn a_business_runs_all_eight_agents_and_public_agents_carry_the_kinds_instruction() {
    let site = Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let inputs = CharterInputs::new(
        site.site.id,
        "Juniper Bakery",
        OWNER,
        "Bread.",
        vec!["en".parse().unwrap()],
        "KES",
    );
    let charter = site.template.draft_charter(&site.team, &inputs);
    site.commit(&charter, &[(Role::Owner, OWNER)]).await;
    let root = tempfile::tempdir().unwrap();
    let public = {
        let (partitions, table, search) =
            (site.partitions.clone(), site.table.clone(), site.search.clone());
        let sink = site.partitions.signal_sink(site.signals.clone(), site.agenda.clone());
        move || adk_harness::website::PublicKit {
            partitions: partitions.clone(),
            table: table.clone(),
            search: search.clone(),
            signals: sink.clone(),
            reply: None,
            capabilities: Vec::new(),
        }
    };
    let kit = TeamKit {
        // Finance, then Growth, each asks for the wallet's status once.
        model: Arc::new(adk_model::ScriptedLlm::new(
            "scripted",
            [true, false]
                .into_iter()
                .flat_map(|_| {
                    [
                        call("wallet_status", serde_json::json!({})),
                        adk_model::ScriptedResponse::text("Noted."),
                    ]
                })
                .collect::<Vec<_>>(),
        )),
        root: root.path().to_path_buf(),
        back_office: site.back_office_kit(&charter),
        public: Arc::new(public),
        standing_rules: site.rules.clone(),
        configure: Arc::new(|_, builder| {
            builder.project_instructions(no_user_file()).without_compaction()
        }),
        wallet: Some(finance_wallet()),
    };
    let built = site.team.build(&kit).await.unwrap();
    assert_eq!(built.roles(), AgentRole::ALL);
    // The Finance agent alone reads and pays from the owner's wallet (spec 3.5 §7.14,
    // §7.19): asked for the wallet, it answers from `wallet_status`; Growth holds no such tool.
    for (role, holds) in [(AgentRole::Finance, true), (AgentRole::Growth, false)] {
        let agent = built.agent(role).unwrap().clone();
        let runner = agent
            .configure_runner(
                adk_runner::Runner::builder()
                    .app_name(site.site.name.clone())
                    .agent(agent.clone())
                    .session_service(site.sessions.clone()),
            )
            .build()
            .unwrap();
        let harness = adk_harness::Harness::with_runner(agent, runner).unwrap();
        let owner = user(OWNER);
        let session = harness.open_session(&owner, None).await.unwrap();
        let events =
            collect(harness.send(&owner, &session, "How does the wallet stand?").await.unwrap())
                .await;
        let status = response(&events, "wallet_status", 0);
        assert_eq!(status.get("currency").is_some(), holds, "{role}: {status}");
    }
    let root = built.compile().expect("the Steward's delegate edges compile over harness agents");
    assert_eq!(adk_core::Agent::name(&root), built.back_office.name);
    let spec = &built.back_office;
    let delegates: Vec<&str> = spec.relationships.iter().map(|r| r.to.as_str()).collect();
    assert_eq!(
        delegates,
        ["growth", "revenue", "guardian", "finance"],
        "the Steward leads the back office"
    );
    assert!(
        spec.members
            .iter()
            .all(|m| !["concierge", "agent-desk", "inbox"].contains(&m.name.as_str())),
        "no edge crosses into the public partition"
    );

    let ministry = SiteTeam::new(site.site.clone(), &template(SiteKind::Government));
    assert!(ministry.public_instruction().contains("End every reply with"));
    assert!(site.team.public_instruction().is_empty(), "a business adds nothing");
}
