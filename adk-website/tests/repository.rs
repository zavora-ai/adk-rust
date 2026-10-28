//! Files and git as an entry point on a served site (spec 3.5 §7.8.2, A-I23, A-I24): a signed
//! push from an authorised committer arrives at the edge's `/hooks/{name}`, becomes a kernel
//! run, reads the changed content files from the pushed commit in a real git repository, and
//! proposes a release that waits for the owner's decision card; a bad signature is refused, a
//! push from an unlisted account becomes a signal only, a replayed delivery is taken once,
//! and a declined push publishes nothing. The sources read a local repository with `git` and
//! a forge's raw endpoint from recorded responses.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use adk_awp::signing::jws::Ed25519DocumentSigner;
use adk_core::principal::SubjectRef;
use adk_core::webhook::{WebhookSecret, WebhookSigner};
use adk_core::{
    ApprovalDecision, ApprovalRequest, ApprovalState, DurableRunStore, InboxQuery, RunFilter,
};
use adk_kernel::charter::{CharterStore, CommitDecision, RoleApproval, SignalFilter, SignalStore};
use adk_site::pipeline::{Change, EntryPoint, PipelineOutcome};
use adk_website::contract::{ContractInputs, SiteKeys, site_documents};
use adk_website::deploy::project::{KeyFile, ProjectPaths};
use adk_website::deploy::{ServeOptions, Served, serve};
use adk_website::onboarding::build::SitePlan;
use adk_website::onboarding::scaffold::{ScaffoldOptions, scaffold};
use adk_website::repository::source::{
    CommitSource, FileChange, HttpCommitSource, LocalGitSource, commit_source_conformance,
};
use adk_website::repository::{PushState, REPOSITORY_RELEASE, REPOSITORY_WORKFLOW};
use adk_website::team::AgentRole;
use adk_website::themes::ThemeId;
use awp_types::{Charter, ContentId, PublicTerms, Role, SiteKind};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;

const OWNER: &str = "owner";
const DOMAIN: &str = "juniper.example";
const GITHUB_SECRET: &str = "juniper github secret";
const GITLAB_TOKEN: &str = "juniper gitlab token";
const PAGE: &str = "content/pages/opening-hours/en.md";

async fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let options = ScaffoldOptions {
        name: "Juniper Bakery".into(),
        domain: DOMAIN.into(),
        kind: SiteKind::Business,
        languages: vec!["en".parse().unwrap()],
        theme: ThemeId::Paper,
        currency: "KES".into(),
    };
    scaffold(dir.path(), &options).await.unwrap();
    dir
}

fn charter(dir: &Path) -> Charter {
    serde_json::from_slice(&std::fs::read(dir.join("charter.draft.json")).unwrap()).unwrap()
}

/// The owner at a terminal delegates the site's release and card keys and writes the signed
/// contract, as `adk-rust awp keys` does.
async fn owner_signs(dir: &Path) {
    let paths = ProjectPaths::new(dir);
    let release = KeyFile::load_or_create(&paths.key_file("release"), "release-2026-10").unwrap();
    let card = KeyFile::load_or_create(&paths.key_file("card"), "card-2026-10").unwrap();
    let keys = SiteKeys {
        root: Arc::new(Ed25519DocumentSigner::from_seed("site-root-2026", [0x11; 32]).unwrap()),
        release: Arc::new(
            Ed25519DocumentSigner::from_seed(release.kid.clone(), release.seed()).unwrap(),
        ),
        edge: Arc::new(Ed25519DocumentSigner::from_seed(card.kid.clone(), card.seed()).unwrap()),
    };
    let terms: PublicTerms =
        serde_json::from_slice(&std::fs::read(dir.join("public-terms.json")).unwrap()).unwrap();
    let inputs = ContractInputs {
        id: charter(dir).site_id,
        name: "Juniper Bakery".into(),
        domain: DOMAIN.into(),
        languages: vec!["en".parse().unwrap()],
        edge: Some(format!("https://{DOMAIN}")),
        release_seq: 1,
        terms,
    };
    let from = chrono::Utc::now() - chrono::Duration::minutes(5);
    let (documents, _) = site_documents(&keys, &inputs, from).await.unwrap();
    for (path, bytes) in documents {
        if path == awp_types::v2::contract::CHARTER_PATH {
            continue;
        }
        let file = paths.well_known().join(path.trim_start_matches("/.well-known/"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, bytes).unwrap();
    }
}

/// `git` in `dir` with a fixed identity, so commits need no user configuration.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Wanjiru", "-c", "user.email=wanjiru@juniper.example"])
        .args(["-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn page(hours: &str) -> String {
    format!(
        "---\nkind: page\ntitle: Opening hours\nsummary: When the bakery is open.\n---\n\
         We are open {hours}.\n"
    )
}

/// The content repository: a first commit with a README, then one adding the opening hours
/// page; returns the two commit ids.
fn content_repository(dir: &Path) -> (String, String) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "--quiet"]);
    std::fs::write(dir.join("README.md"), "The bakery's site.\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "--quiet", "-m", "start"]);
    let first = git(dir, &["rev-parse", "HEAD"]);
    std::fs::create_dir_all(dir.join("content/pages/opening-hours")).unwrap();
    std::fs::write(dir.join(PAGE), page("from seven to seven")).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "--quiet", "-m", "opening hours"]);
    (first, git(dir, &["rev-parse", "HEAD"]))
}

/// `repository.json` with a GitHub hook, a GitLab hook and a signed hook, their secrets in
/// files beside the project, and two committers.
fn configure(dir: &Path, newsdesk: &WebhookSecret) {
    std::fs::create_dir_all(dir.join("secrets")).unwrap();
    std::fs::write(dir.join("secrets/github"), GITHUB_SECRET).unwrap();
    std::fs::write(dir.join("secrets/gitlab"), GITLAB_TOKEN).unwrap();
    std::fs::write(dir.join("secrets/newsdesk"), newsdesk.to_whsec()).unwrap();
    let config = json!({
        "branch": "main",
        "source": { "git": "content-repo" },
        "hooks": [
            { "name": "github", "forge": "github", "secret": "file:secrets/github" },
            { "name": "gitlab", "forge": "gitlab", "secret": "file:secrets/gitlab" },
            { "name": "newsdesk", "forge": "signed", "secret": "file:secrets/newsdesk" }
        ],
        "committers": [
            { "id": "wanjiru", "account": "github:wanjiru-k" },
            { "id": "newsdesk", "key": "newsdesk" }
        ]
    });
    std::fs::write(dir.join("repository.json"), serde_json::to_vec_pretty(&config).unwrap())
        .unwrap();
}

async fn start(dir: &Path) -> Served {
    let mut options = ServeOptions::new(dir, OWNER);
    options.edge = "127.0.0.1:0".parse().unwrap();
    options.kernel = "127.0.0.1:0".parse().unwrap();
    options.site.idle = Some(Duration::from_millis(200));
    serve(options).await.unwrap()
}

/// Commits the drafted charter and publishes release 1 through the site's pipeline.
async fn launch(served: &Served, dir: &Path) {
    let charter = charter(dir);
    let decision = CommitDecision::new("dec-1", chrono::Utc::now()).with_approval(
        RoleApproval::new(Role::Owner, SubjectRef::human(OWNER), chrono::Utc::now()),
    );
    served.stores.charters.commit(charter.clone(), decision).await.unwrap();
    let site = &served.sites[0];
    let change = Change {
        items: SitePlan::new(&charter, "Juniper Bakery").items(&charter, &[], &["KE".into()]),
        media: Vec::new(),
        entry: EntryPoint::SiteAgent { agent: AgentRole::Steward.principal(&site.site) },
        proposer: AgentRole::Steward.principal(&site.site),
        job: "onboarding".into(),
        action_class: "publish".into(),
    };
    let outcome = site.runtime.pipeline().run(change).await.unwrap();
    assert!(matches!(outcome, PipelineOutcome::Published(_)), "{outcome:?}");
}

fn push(before: &str, after: &str, sender: &str, files: &[&str]) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "ref": "refs/heads/main",
        "before": before,
        "after": after,
        "repository": { "full_name": "juniper/site" },
        "pusher": { "name": sender },
        "sender": { "login": sender },
        "commits": [{ "id": after, "added": files, "modified": [], "removed": [] }]
    }))
    .unwrap()
}

fn github_signature(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// Posts `body` to the edge's hook `name` with `headers`: the status and the answer.
async fn post(
    addr: SocketAddr,
    name: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> (u16, Value) {
    let mut request = reqwest::Client::new()
        .post(format!("http://{addr}/hooks/{name}"))
        .header("content-type", "application/json")
        .body(body.to_vec());
    for (header, value) in headers {
        request = request.header(*header, value);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

async fn github(addr: SocketAddr, delivery: &str, secret: &str, body: &[u8]) -> (u16, Value) {
    let headers = [
        ("x-hub-signature-256", github_signature(secret, body)),
        ("x-github-delivery", delivery.to_string()),
        ("x-github-event", "push".to_string()),
    ];
    post(addr, "github", &headers, body).await
}

/// Waits for the push run `run_id` to end; its output.
async fn finished(served: &Served, run_id: &str) -> Value {
    for _ in 0..200 {
        let runs = served.stores.runs.list(RunFilter::default()).await.unwrap();
        if let Some(run) = runs.iter().find(|run| run.run_id == run_id && run.state.is_terminal()) {
            assert_eq!(run.target, REPOSITORY_WORKFLOW);
            assert_eq!(run.error, None, "{run:#?}");
            return run.result.clone().unwrap_or(Value::Null);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let runs = served.stores.runs.list(RunFilter::default()).await.unwrap();
    panic!("the push run {run_id} did not end: {runs:#?}");
}

async fn cards(served: &Served) -> Vec<ApprovalRequest> {
    let inbox = served
        .stores
        .approvals
        .inbox(InboxQuery::for_approver(SubjectRef::human(OWNER), Vec::<String>::new()))
        .await
        .unwrap();
    inbox.items.into_iter().filter(|card| card.tool_name == REPOSITORY_RELEASE).collect()
}

/// Waits until the entry's record of `commit` reaches `state`.
async fn reaches(served: &Served, commit: &str, state: PushState) {
    let entry = served.sites[0].repository.clone().expect("the site has a repository entry");
    for _ in 0..200 {
        if entry.records().await.iter().any(|r| r.commit == commit && r.state == state) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{commit} never reached {state:?}: {:#?}", entry.records().await);
}

fn page_id() -> ContentId {
    ContentId("pages/opening-hours/en".into())
}

async fn status_of(addr: SocketAddr, path: &str) -> u16 {
    reqwest::get(format!("http://{addr}{path}")).await.unwrap().status().as_u16()
}

#[tokio::test]
async fn a_signed_push_from_an_authorised_committer_waits_for_the_owner_then_publishes() {
    let dir = project().await;
    owner_signs(dir.path()).await;
    let (first, second) = content_repository(&dir.path().join("content-repo"));
    configure(dir.path(), &WebhookSecret::from_bytes(b"newsdesk key".to_vec()).unwrap());
    let served = start(dir.path()).await;
    launch(&served, dir.path()).await;
    let edge = served.edge_addr;
    let body = push(&first, &second, "wanjiru-k", &[PAGE]);

    // Bad signatures are refused before anything runs.
    assert_eq!(github(edge, "d-forged", "another secret", &body).await.0, 401);
    let token = [("x-gitlab-token", "a guess".to_string()), ("x-gitlab-event-uuid", "e-1".into())];
    assert_eq!(post(edge, "gitlab", &token, &body).await.0, 401);
    let stranger = WebhookSigner::new(WebhookSecret::from_bytes(b"not the key".to_vec()).unwrap());
    let headers: Vec<(&str, String)> =
        stranger.sign_now("s-1", &body).header_pairs().into_iter().collect();
    assert_eq!(post(edge, "newsdesk", &headers, &body).await.0, 401);
    assert!(
        served
            .stores
            .runs
            .list(RunFilter::default())
            .await
            .unwrap()
            .iter()
            .all(|run| run.target != REPOSITORY_WORKFLOW)
    );

    // A good push proposes a release: a card for the owner, nothing published yet.
    let (status, accepted) = github(edge, "d-1", GITHUB_SECRET, &body).await;
    assert_eq!(status, 202, "{accepted}");
    let output = finished(&served, accepted["runId"].as_str().unwrap()).await;
    assert_eq!(output["outcome"], "taken", "{output}");
    assert_eq!(output["state"], "proposed", "{output}");
    assert_eq!(output["committer"], "wanjiru");
    assert_eq!(output["items"], json!(["pages/opening-hours/en"]));
    let pending = cards(&served).await;
    assert_eq!(pending.len(), 1, "{pending:#?}");
    assert!(pending[0].summary.contains("wanjiru pushed"), "{}", pending[0].summary);
    assert_eq!(pending[0].args_redacted["commit"], json!(second));
    assert_eq!(
        status_of(edge, "/opening-hours/").await,
        404,
        "nothing publishes before the owner decides"
    );

    // A replayed delivery is taken once: the same id answers the same run, and the same push
    // under another id proposes nothing again.
    let (status, again) = github(edge, "d-1", GITHUB_SECRET, &body).await;
    assert_eq!(
        (status, &again["duplicate"], &again["runId"]),
        (200, &json!(true), &accepted["runId"])
    );
    let (status, replayed) = github(edge, "d-1-replayed", GITHUB_SECRET, &body).await;
    assert_eq!(status, 202);
    let output = finished(&served, replayed["runId"].as_str().unwrap()).await;
    assert_eq!(output, json!({ "outcome": "duplicate", "commit": second }));
    assert_eq!(cards(&served).await.len(), 1);

    // The owner approves on the card: the release goes out through the site's pipeline.
    let card = &pending[0];
    served
        .stores
        .approvals
        .decide(&card.id, card.version, ApprovalDecision::approve(SubjectRef::human(OWNER)))
        .await
        .unwrap();
    reaches(&served, &second, PushState::Published).await;
    assert_eq!(status_of(edge, "/opening-hours/").await, 200);
    let record = served.sites[0].repository.clone().unwrap().records().await;
    assert_eq!(record.iter().filter(|r| r.commit == second).count(), 1, "{record:#?}");
    assert_eq!(record[0].release, Some(2));
    let journal = std::fs::read_to_string(dir.path().join("data/repository/pushes.json")).unwrap();
    assert!(journal.contains("\"published\""), "{journal}");
    served.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_unlisted_committer_files_a_signal_and_a_declined_push_publishes_nothing() {
    let dir = project().await;
    owner_signs(dir.path()).await;
    let (first, second) = content_repository(&dir.path().join("content-repo"));
    let newsdesk = WebhookSecret::from_bytes(b"newsdesk key".to_vec()).unwrap();
    configure(dir.path(), &newsdesk);
    let served = start(dir.path()).await;
    launch(&served, dir.path()).await;
    let edge = served.edge_addr;

    // A push by an account nobody listed is a signal for the back office, and authors nothing.
    let body = push(&first, &second, "mallory", &[PAGE]);
    let (status, accepted) = github(edge, "d-mallory", GITHUB_SECRET, &body).await;
    assert_eq!(status, 202, "{accepted}");
    let output = finished(&served, accepted["runId"].as_str().unwrap()).await;
    assert_eq!(
        (output["state"].as_str(), output.get("committer")),
        (Some("signalled"), None),
        "{output}"
    );
    assert!(cards(&served).await.is_empty());
    let signals =
        served.stores.signals.list(&SignalFilter::default().kind("inbound_message")).await.unwrap();
    assert_eq!(signals.len(), 1, "{signals:#?}");
    let filed = serde_json::to_string(&signals[0]).unwrap();
    assert!(!filed.contains("mallory") && !filed.contains("opening"), "typed fields only: {filed}");
    assert!(served.sites[0].runtime.store().get(&page_id()).await.unwrap().is_none());
    let (status, replayed) = github(edge, "d-mallory-again", GITHUB_SECRET, &body).await;
    assert_eq!(status, 202, "{replayed}");
    let output = finished(&served, replayed["runId"].as_str().unwrap()).await;
    assert_eq!(output["outcome"], "duplicate", "a replayed signal is filed once");

    // The same commit from the newsdesk's own key, in the plain signed form any host sends,
    // proposes a release; the owner declines it.
    let signer = WebhookSigner::new(newsdesk);
    let signed = serde_json::to_vec(&json!({
        "repository": "juniper/site", "ref": "refs/heads/main", "before": first, "after": second,
        "commits": [{ "id": second, "added": [PAGE], "modified": [], "removed": [] }]
    }))
    .unwrap();
    let headers: Vec<(&str, String)> =
        signer.sign_now("n-1", &signed).header_pairs().into_iter().collect();
    let (status, accepted) = post(edge, "newsdesk", &headers, &signed).await;
    assert_eq!(status, 202, "{accepted}");
    let output = finished(&served, accepted["runId"].as_str().unwrap()).await;
    assert_eq!(
        (output["state"].as_str(), output["committer"].as_str()),
        (Some("proposed"), Some("newsdesk"))
    );
    let card = cards(&served).await.pop().expect("a card for the owner");
    served
        .stores
        .approvals
        .decide(
            &card.id,
            card.version,
            ApprovalDecision::reject(SubjectRef::human(OWNER), "not yet"),
        )
        .await
        .unwrap();
    reaches(&served, &second, PushState::Declined).await;
    let decided = served.stores.approvals.get(&card.id).await.unwrap().unwrap();
    assert_eq!(decided.state, ApprovalState::Rejected);
    assert_eq!(status_of(edge, "/opening-hours/").await, 404, "a declined push publishes nothing");
    assert!(served.sites[0].runtime.store().get(&page_id()).await.unwrap().is_none());
    served.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_local_repository_reads_files_and_lists_a_pushs_changes() {
    let dir = tempfile::tempdir().unwrap();
    let (first, second) = content_repository(dir.path());
    let source = LocalGitSource::new(dir.path());
    commit_source_conformance(
        &source,
        "juniper/site",
        &second,
        (PAGE, page("from seven to seven").as_bytes()),
        "content/pages/missing/en.md",
    )
    .await;
    assert_eq!(
        source.read("juniper/site", &first, PAGE).await.unwrap(),
        None,
        "not in the first commit"
    );
    let changes = source.changes("juniper/site", Some(&first), &second).await.unwrap();
    assert_eq!(changes, Some(vec![FileChange { path: PAGE.into(), removed: false }]));
    assert!(source.read("juniper/site", &"f".repeat(40), PAGE).await.is_err(), "an unknown commit");

    // A mirror fetches a commit it does not have yet from its remote.
    let mirror = tempfile::tempdir().unwrap();
    git(mirror.path(), &["init", "--quiet", "--bare"]);
    git(mirror.path(), &["remote", "add", "origin", dir.path().to_str().unwrap()]);
    let fetching = LocalGitSource::new(mirror.path()).with_fetch("origin");
    let read = fetching.read("juniper/site", &second, PAGE).await.unwrap();
    assert_eq!(read.as_deref(), Some(page("from seven to seven").as_bytes()));
}

/// The forge's raw endpoint, answered from recorded responses: the file at the commit, and
/// GitHub's `404: Not Found` for a path the commit does not hold.
#[tokio::test]
async fn a_forges_raw_endpoint_reads_files_from_recorded_responses() {
    use axum::extract::Path as UrlPath;
    use axum::http::{HeaderMap, StatusCode};

    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/repository");
    let recorded: BTreeMap<String, String> =
        serde_json::from_slice(&std::fs::read(fixtures.join("github-raw.json")).unwrap()).unwrap();
    let commit = recorded["commit"].clone();
    let body = std::fs::read(fixtures.join(&recorded["body"])).unwrap();
    let expected = format!("/juniper/site/{commit}/{PAGE}");
    let app = axum::Router::new().route(
        "/{*path}",
        axum::routing::get(move |UrlPath(path): UrlPath<String>, headers: HeaderMap| {
            let (body, expected) = (body.clone(), expected.clone());
            async move {
                assert_eq!(headers["authorization"], "Bearer recorded-token");
                if format!("/{path}") == expected {
                    (StatusCode::OK, body)
                } else {
                    (StatusCode::NOT_FOUND, b"404: Not Found".to_vec())
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let source = HttpCommitSource::new(format!("http://{addr}/{{repository}}/{{commit}}/{{path}}"))
        .unwrap()
        .with_auth("Authorization", adk_core::CredentialSecret::new("Bearer recorded-token"))
        .unwrap();
    let recorded_page = std::fs::read(fixtures.join(&recorded["body"])).unwrap();
    commit_source_conformance(
        &source,
        "juniper/site",
        &commit,
        (PAGE, &recorded_page),
        "content/pages/missing/en.md",
    )
    .await;
    assert_eq!(source.changes("juniper/site", None, &commit).await.unwrap(), None);
    server.abort();
}
