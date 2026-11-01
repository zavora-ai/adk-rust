//! Files and git as an entry point (spec 3.5 §7.8.2 "Entry points", A-I23, A-I24): a push
//! from an authorised committer to the content repository arrives through a signed webhook
//! and proposes a release, which waits at the site's normal approval.
//!
//! ```text
//! forge ──signed push──▶ /hooks/{name} (adk-server's WebhookIngress, keyed by delivery id)
//!   ──durable run──▶ the site's kernel ──REPOSITORY_WORKFLOW──▶ RepositoryEntry::receive
//!   ──▶ a listed committer?
//!         no  ──▶ one `inbound_message` signal; nothing is authored (A-I24)
//!         yes ──▶ CommitSource reads the changed content files at the pushed commit
//!             ──▶ the pipeline: draft, the per-change checks, the preview with page diffs
//!             ──▶ the owner's decision card ──approved──▶ the approval chain (A-I23),
//!                 release, publish, announce, measure
//! ```
//!
//! | Part | Does |
//! |---|---|
//! | [`RepositoryConfig`] | `repository.json`: the branch, where files are read, the signed hooks and the authorised committers |
//! | [`PushEvent`] | A push as GitHub, GitLab or the plain signed form describes it |
//! | [`source`] | [`source::CommitSource`]: files read from the pushed commit; a local git repository or a forge's raw endpoint over HTTP |
//! | [`RepositoryEntry`] | Authorises the pusher, drafts and previews the change, cards it for the owner, and applies the owner's decision |
//!
//! | Hook | Authenticated by | The committer is |
//! |---|---|---|
//! | `github` | `X-Hub-Signature-256` with the hook's secret | The listed `account` that pushed (`sender.login`, else `pusher.name`) |
//! | `gitlab` | `X-Gitlab-Token` with the hook's secret | The listed `account` that pushed (`user_username`) |
//! | `signed` | Standard Webhooks with the hook's `whsec_` key, from any host | The one committer whose `key` the hook is |
//!
//! A push is taken once: the ingress keys its run by the delivery id, and the entry keeps
//! every commit it has taken in `data/repository/pushes.json`, so a delivery replayed under
//! another id proposes nothing again; a commit only signalled stays open to a listed
//! committer's push. Only the configured branch proposes releases, and only
//! content files (`content/<kind>/<slug>/<language>.md`) are read; a removed one withdraws
//! its item (410 Gone).

pub mod source;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalService,
    ApprovalState, ApproverSelector, DurableRunStore, NewRun, Result, ToolConfirmationRequest,
};
use adk_harness::website::{SiteRef, SiteRuntime};
use adk_kernel::QueueClass;
use adk_server::awp::SignalEmitter;
use adk_server::webhooks::{
    ForgeSecret, ForgeSignature, WebhookHook, WebhookIngress, webhook_ingress_router,
};
use adk_site::content::{CONTENT_DIR, parse_content_file};
use adk_site::pipeline::{ApprovalDecision, Change, Draft, EntryPoint, PipelineOutcome, Preview};
use awp_types::{
    Channel, ContentId, ContentItem, ContentState, MessageCategory, Signal, SignalKind, ThreadRef,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use self::source::{CommitSource, FileChange, HttpCommitSource, LocalGitSource, is_commit_id};
use crate::deploy::project::{ProjectPaths, read_json, secret_in};

/// The workflow a push's run executes in the site's kernel (spec 3.5 §7.8.2).
pub const REPOSITORY_WORKFLOW: &str = "website.repository.push";
/// The tool a push's decision card names.
pub const REPOSITORY_RELEASE: &str = "site_repository_release";
/// The job a push's release records as its source.
pub const REPOSITORY_JOB: &str = "repository";
/// The project file that configures the entry point.
pub const REPOSITORY_FILE: &str = "repository.json";
/// The most push records `data/repository/pushes.json` keeps.
const KEPT_RECORDS: usize = 5_000;

fn config(error: impl std::fmt::Display) -> AdkError {
    AdkError::config(error.to_string())
}

fn default_branch() -> String {
    "main".to_string()
}

/// How a hook's deliveries are signed (spec 3.5 §7.8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Forge {
    /// GitHub's push webhook, `X-Hub-Signature-256`.
    Github,
    /// GitLab's push webhook, `X-Gitlab-Token`.
    Gitlab,
    /// The plain signed form any host can send: Standard Webhooks over a [`PushEvent`]
    /// document.
    Signed,
}

/// One signed webhook, served at `/hooks/{name}` on the site's edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookConfig {
    /// The hook's name: letters, digits, `-` and `_`.
    pub name: String,
    /// How its deliveries are signed.
    pub forge: Forge,
    /// Its secret, as `env:VAR` or `file:PATH`: the forge's webhook secret, or a Standard
    /// Webhooks `whsec_` key for `signed`.
    pub secret: String,
}

/// An authorised committer: a forge account, or the key of a signed hook (spec 3.5 A-I24).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitterConfig {
    /// The person, as the charter's governance names them: the proposer of the releases
    /// their pushes make, who cannot also approve them under an approval chain (A-I23).
    pub id: String,
    /// A forge account, `{hook}:{username}`: that account's pushes on that hook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// A `signed` hook whose key only this committer holds: every push on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// A credential the HTTP source sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceAuth {
    /// The header, such as `Authorization` or `PRIVATE-TOKEN`.
    pub header: String,
    /// Its whole value, as `env:VAR` or `file:PATH`.
    pub value: String,
}

/// Where the pushed commit's files are read: a local git repository or a forge's raw
/// endpoint ([`source`]).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceConfig {
    /// A git repository on this machine, relative to the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<PathBuf>,
    /// The remote the local repository fetches a missing commit from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch: Option<String>,
    /// A raw-file URL template ([`HttpCommitSource`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<String>,
    /// The credential the HTTP source sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<SourceAuth>,
}

/// `repository.json`: files and git as an entry point (spec 3.5 §7.8.2, A-I24).
///
/// ```json
/// {
///   "branch": "main",
///   "source": { "git": "../juniper-content", "fetch": "origin" },
///   "hooks": [
///     { "name": "github", "forge": "github", "secret": "env:JUNIPER_GITHUB_WEBHOOK_SECRET" },
///     { "name": "studio", "forge": "signed", "secret": "env:JUNIPER_STUDIO_WHSEC" }
///   ],
///   "committers": [
///     { "id": "wanjiru", "account": "github:wanjiru-k" },
///     { "id": "newsdesk", "key": "studio" }
///   ]
/// }
/// ```
///
/// # Example
///
/// ```rust
/// use adk_website::repository::RepositoryConfig;
///
/// let config: RepositoryConfig = serde_json::from_str(r#"{
///   "source": { "http": "https://raw.githubusercontent.com/{repository}/{commit}/{path}" },
///   "hooks": [{ "name": "github", "forge": "github", "secret": "env:SECRET" }],
///   "committers": [{ "id": "wanjiru", "account": "github:wanjiru-k" }]
/// }"#).unwrap();
/// assert_eq!(config.branch, "main");
/// assert!(config.validate().is_ok());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepositoryConfig {
    /// The branch whose pushes propose releases; `main` by default.
    #[serde(default = "default_branch")]
    pub branch: String,
    /// Where the pushed commit's files are read.
    pub source: SourceConfig,
    /// The signed webhooks.
    pub hooks: Vec<HookConfig>,
    /// The authorised committers; anyone else's push becomes a signal.
    pub committers: Vec<CommitterConfig>,
}

impl RepositoryConfig {
    /// Checks the hooks, the committers and the source.
    ///
    /// # Errors
    ///
    /// A hook name that is not a path segment or is listed twice; a committer listed twice,
    /// with both or neither of `account` and `key`, an account on a hook that is not a
    /// forge's, a key that is not a `signed` hook or is held by two committers; a source
    /// with both or neither of `git` and `http`.
    pub fn validate(&self) -> Result<()> {
        let mut hooks = BTreeMap::new();
        for hook in &self.hooks {
            let valid = !hook.name.is_empty()
                && hook.name.len() <= 64
                && hook.name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
            if !valid {
                return Err(config(format!(
                    "{REPOSITORY_FILE}: hook `{}` needs a name of letters, digits, `-` and `_`; \
                     it is served at /hooks/<name>",
                    hook.name
                )));
            }
            if hooks.insert(hook.name.as_str(), hook.forge).is_some() {
                return Err(config(format!(
                    "{REPOSITORY_FILE}: hook `{}` is listed twice",
                    hook.name
                )));
            }
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut keys = std::collections::BTreeSet::new();
        for committer in &self.committers {
            if committer.id.trim().is_empty() || !ids.insert(committer.id.as_str()) {
                return Err(config(format!(
                    "{REPOSITORY_FILE}: committer `{}` needs an id listed once",
                    committer.id
                )));
            }
            match (&committer.account, &committer.key) {
                (Some(account), None) => {
                    let forge = account
                        .split_once(':')
                        .filter(|(_, user)| !user.is_empty())
                        .and_then(|(hook, _)| hooks.get(hook));
                    if !matches!(forge, Some(Forge::Github | Forge::Gitlab)) {
                        return Err(config(format!(
                            "{REPOSITORY_FILE}: committer `{}`'s account `{account}` is not \
                             `<hook>:<username>` on a github or gitlab hook",
                            committer.id
                        )));
                    }
                }
                (None, Some(key)) => {
                    if hooks.get(key.as_str()) != Some(&Forge::Signed) || !keys.insert(key.as_str())
                    {
                        return Err(config(format!(
                            "{REPOSITORY_FILE}: committer `{}`'s key `{key}` must be a signed \
                             hook no other committer holds",
                            committer.id
                        )));
                    }
                }
                _ => {
                    return Err(config(format!(
                        "{REPOSITORY_FILE}: committer `{}` needs an `account` or a `key`",
                        committer.id
                    )));
                }
            }
        }
        if self.source.git.is_some() == self.source.http.is_some() {
            return Err(config(format!(
                "{REPOSITORY_FILE}: the source is either `git` (a local repository) or `http` (a \
                 raw-file URL template)"
            )));
        }
        Ok(())
    }

    /// The committer of `push` on `hook`: the listed account that pushed on a forge's hook,
    /// the key's holder on a signed one.
    pub fn committer(&self, hook: &HookConfig, push: &PushEvent) -> Option<&CommitterConfig> {
        match hook.forge {
            Forge::Signed => {
                self.committers.iter().find(|c| c.key.as_deref() == Some(hook.name.as_str()))
            }
            Forge::Github | Forge::Gitlab => {
                let pusher = push.pusher.as_deref()?;
                let account = format!("{}:{pusher}", hook.name);
                self.committers.iter().find(|c| c.account.as_deref() == Some(account.as_str()))
            }
        }
    }
}

/// A push to the content repository (spec 3.5 §7.8.2), as GitHub, GitLab or the plain
/// signed form describe it.
///
/// The plain signed form is GitHub's shape, reduced:
///
/// ```json
/// { "repository": "juniper/site", "ref": "refs/heads/main",
///   "before": "<commit id>", "after": "<commit id>",
///   "commits": [{ "id": "<commit id>", "added": [], "modified": ["content/pages/hours/en.md"], "removed": [] }] }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushEvent {
    /// The repository's name, such as `juniper/site`.
    pub repository: String,
    /// The branch pushed to; `None` for a tag or another ref.
    pub branch: Option<String>,
    /// The commit before the push, when the branch existed.
    pub before: Option<String>,
    /// The pushed commit; `None` when the push deletes the branch.
    pub commit: Option<String>,
    /// The forge account that pushed.
    pub pusher: Option<String>,
    /// The files the push's commits change, each once with its last change.
    pub files: Vec<FileChange>,
}

impl PushEvent {
    /// Reads a push from `payload` as `forge` sends it; `None` when it is not a push.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_website::repository::{Forge, PushEvent};
    /// use serde_json::json;
    ///
    /// let sha = "a".repeat(40);
    /// let push = PushEvent::parse(Forge::Github, &json!({
    ///     "ref": "refs/heads/main", "before": "0".repeat(40), "after": sha,
    ///     "repository": { "full_name": "juniper/site" },
    ///     "pusher": { "name": "wanjiru-k" }, "sender": { "login": "wanjiru-k" },
    ///     "commits": [{ "id": sha, "added": ["content/pages/hours/en.md"], "modified": [], "removed": [] }]
    /// })).unwrap();
    /// assert_eq!((push.branch.as_deref(), push.pusher.as_deref()), (Some("main"), Some("wanjiru-k")));
    /// assert_eq!(push.before, None, "a new branch has no commit before");
    /// assert!(PushEvent::parse(Forge::Github, &json!({ "zen": "ping" })).is_none());
    /// ```
    pub fn parse(forge: Forge, payload: &Value) -> Option<Self> {
        let text = |pointer: &str| {
            payload
                .pointer(pointer)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        if forge == Forge::Gitlab && text("/object_kind").is_some_and(|kind| kind != "push") {
            return None;
        }
        let reference = text("/ref")?;
        let after = text("/after")?;
        payload.get("commits").and_then(Value::as_array)?;
        let commit_id = |id: Option<&str>| {
            id.filter(|id| is_commit_id(id) && id.bytes().any(|b| b != b'0')).map(str::to_string)
        };
        let deleted = payload.get("deleted").and_then(Value::as_bool) == Some(true);
        let repository = match forge {
            Forge::Github => text("/repository/full_name"),
            Forge::Gitlab => text("/project/path_with_namespace"),
            Forge::Signed => text("/repository")
                .or_else(|| text("/repository/full_name"))
                .or_else(|| text("/repository/name")),
        }
        .unwrap_or_default()
        .to_string();
        let pusher = match forge {
            Forge::Github => text("/sender/login").or_else(|| text("/pusher/name")),
            Forge::Gitlab => text("/user_username"),
            Forge::Signed => None,
        }
        .map(str::to_string);
        let mut last: BTreeMap<String, (usize, bool)> = BTreeMap::new();
        let mut order = 0;
        for commit in payload["commits"].as_array().into_iter().flatten() {
            for (list, removed) in [("added", false), ("modified", false), ("removed", true)] {
                for path in commit.get(list).and_then(Value::as_array).into_iter().flatten() {
                    if let Some(path) = path.as_str() {
                        order += 1;
                        last.insert(path.to_string(), (order, removed));
                    }
                }
            }
        }
        let mut files: Vec<(usize, FileChange)> = last
            .into_iter()
            .map(|(path, (order, removed))| (order, FileChange { path, removed }))
            .collect();
        files.sort();
        Some(Self {
            repository,
            branch: reference.strip_prefix("refs/heads/").map(str::to_string),
            before: commit_id(text("/before")),
            commit: if deleted { None } else { commit_id(Some(after)) },
            pusher,
            files: files.into_iter().map(|(_, file)| file).collect(),
        })
    }
}

/// What became of a push (spec 3.5 §7.8.2, A-I24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushState {
    /// Not for publishing: no content file changed.
    Ignored,
    /// From no authorised committer: one signal, nothing authored.
    Signalled,
    /// A file that is not content, or the release refused it (A-I1).
    Refused,
    /// The per-change checks held it.
    Held,
    /// Waiting for the owner's decision card.
    Proposed,
    /// Approved by the owner, waiting for the rest of the approval chain (A-I23).
    Approving,
    /// Released and published.
    Published,
    /// Saved to publish at its `publish_at`.
    Scheduled,
    /// Declined, or the card expired.
    Declined,
}

/// One push the entry took, kept in `data/repository/pushes.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushRecord {
    /// The hook it arrived on.
    pub hook: String,
    /// The delivery id.
    pub delivery: String,
    /// The repository.
    pub repository: String,
    /// The branch.
    pub branch: String,
    /// The pushed commit.
    pub commit: String,
    /// The authorised committer, when one pushed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committer: Option<String>,
    /// When it arrived.
    pub at: DateTime<Utc>,
    /// What became of it.
    pub state: PushState,
    /// The owner's decision card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
    /// The items it changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ContentId>,
    /// Why it was refused or held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The release it published in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<u64>,
}

/// What [`RepositoryEntry::receive`] did with one delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum PushOutcome {
    /// Not a push to the site's branch, or no content changed.
    Ignored {
        /// Why.
        reason: String,
    },
    /// The commit was taken before: a replayed delivery.
    Duplicate {
        /// The commit.
        commit: String,
    },
    /// Taken: the record says what became of it.
    Taken(Box<PushRecord>),
}

/// What [`RepositoryEntry::open`] builds the entry from beyond `repository.json`.
pub struct RepositoryParts {
    /// The site.
    pub site: SiteRef,
    /// The person whose decision cards approve releases.
    pub owner: String,
    /// The runtime whose pipeline releases.
    pub runtime: Arc<SiteRuntime>,
    /// Where decision cards go: the kernel's approval service.
    pub approvals: Arc<dyn ApprovalService>,
    /// Where an unauthorised push's signal goes: the back office.
    pub signals: Arc<dyn SignalEmitter>,
    /// The source, when not the one `repository.json` names.
    pub source: Option<Arc<dyn CommitSource>>,
}

/// A change waiting for its decision, with the preview the card names.
struct Pending {
    draft: Draft,
    preview: Preview,
}

/// Files and git as an entry point for one site (spec 3.5 §7.8.2, A-I23, A-I24).
pub struct RepositoryEntry {
    parts: RepositoryParts,
    config: RepositoryConfig,
    source: Arc<dyn CommitSource>,
    hooks: Vec<WebhookHook>,
    journal: PathBuf,
    records: Mutex<Vec<PushRecord>>,
    pending: Mutex<BTreeMap<String, Pending>>,
}

impl std::fmt::Debug for RepositoryEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepositoryEntry")
            .field("site", &self.parts.site)
            .field("branch", &self.config.branch)
            .finish_non_exhaustive()
    }
}

impl RepositoryEntry {
    /// The entry of the project at `paths`, from its `repository.json`; `None` without the
    /// file. Each hook's secret is read now.
    ///
    /// # Errors
    ///
    /// A file that does not parse or [validate](RepositoryConfig::validate), a secret written
    /// in the file or not found, or a source that cannot be built.
    pub fn open(paths: &ProjectPaths, parts: RepositoryParts) -> Result<Option<Self>> {
        let Some(settings) = read_json::<RepositoryConfig>(&paths.root().join(REPOSITORY_FILE))?
        else {
            return Ok(None);
        };
        settings.validate()?;
        let source: Arc<dyn CommitSource> = match (&parts.source, &settings.source) {
            (Some(source), _) => source.clone(),
            (None, SourceConfig { git: Some(dir), fetch, .. }) => {
                let mut source = LocalGitSource::new(paths.root().join(dir));
                if let Some(remote) = fetch {
                    source = source.with_fetch(remote.clone());
                }
                Arc::new(source)
            }
            (None, SourceConfig { http: Some(template), auth, .. }) => {
                let mut source = HttpCommitSource::new(template.clone())?;
                if let Some(auth) = auth {
                    let value = secret_in(paths, REPOSITORY_FILE, &auth.value)?;
                    source = source.with_auth(&auth.header, value)?;
                }
                Arc::new(source)
            }
            (None, _) => return Err(config("repository.json names no source")),
        };
        let app = parts.site.name.clone();
        let site = parts.site.id;
        let mut hooks = Vec::new();
        for hook in &settings.hooks {
            let secret = secret_in(paths, REPOSITORY_FILE, &hook.secret)?;
            let mut served = match hook.forge {
                Forge::Github => WebhookHook::forge(
                    &hook.name,
                    ForgeSignature::GitHub(ForgeSecret::new(secret.expose()).map_err(config)?),
                    &app,
                    REPOSITORY_WORKFLOW,
                ),
                Forge::Gitlab => WebhookHook::forge(
                    &hook.name,
                    ForgeSignature::GitLab(ForgeSecret::new(secret.expose()).map_err(config)?),
                    &app,
                    REPOSITORY_WORKFLOW,
                ),
                Forge::Signed => {
                    let key = adk_core::webhook::WebhookSecret::from_base64(secret.expose())
                        .map_err(|e| {
                            config(format!("{REPOSITORY_FILE}: hook `{}`: {e}", hook.name))
                        })?;
                    WebhookHook::new(
                        &hook.name,
                        adk_core::webhook::WebhookVerifier::new(key),
                        &app,
                        REPOSITORY_WORKFLOW,
                    )
                }
            };
            let app = app.clone();
            served = served.with_queue(QueueClass::BACKGROUND).with_max_pending(100).with_mapper(
                move |delivery| {
                    Ok(NewRun::new(
                        QueueClass::BACKGROUND,
                        app.clone(),
                        format!("repository:{}", delivery.hook),
                        REPOSITORY_WORKFLOW,
                        json!({
                            "site": site,
                            "hook": delivery.hook,
                            "delivery": delivery.webhook.id,
                            "payload": delivery.payload,
                        }),
                    ))
                },
            );
            hooks.push(served);
        }
        let journal = paths.data().join("repository").join("pushes.json");
        let records: Vec<PushRecord> = read_json(&journal)?.unwrap_or_default();
        Ok(Some(Self {
            parts,
            config: settings,
            source,
            hooks,
            journal,
            records: Mutex::new(records),
            pending: Mutex::new(BTreeMap::new()),
        }))
    }

    /// The configuration.
    pub fn config(&self) -> &RepositoryConfig {
        &self.config
    }

    /// The pushes taken so far, oldest first.
    pub async fn records(&self) -> Vec<PushRecord> {
        self.records.lock().await.clone()
    }

    /// The hooks at `/hooks/{name}`: `adk-server`'s webhook ingress over `runs`, the store
    /// the site's kernel claims runs from, so each push is one durable run keyed by its
    /// delivery id.
    pub fn router(&self, runs: Arc<dyn DurableRunStore>) -> axum::Router {
        let ingress =
            self.hooks.iter().cloned().fold(WebhookIngress::new(runs), WebhookIngress::with_hook);
        webhook_ingress_router(ingress)
    }

    /// Runs one push's workflow input: `{site, hook, delivery, payload}`.
    ///
    /// # Errors
    ///
    /// As [`receive`](Self::receive), or an input without those members.
    pub async fn execute(&self, input: &adk_server::background::WorkflowState) -> Result<Value> {
        let text = |key: &str| {
            input
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| config(format!("a {REPOSITORY_WORKFLOW} run names its `{key}`")))
        };
        let outcome = self
            .receive(text("hook")?, text("delivery")?, input.get("payload").unwrap_or(&Value::Null))
            .await?;
        serde_json::to_value(outcome).map_err(config)
    }

    /// Takes one verified delivery (spec 3.5 §7.8.2, A-I24): ignores what is not a push to
    /// the site's branch, answers a commit taken before as a duplicate, files a signal for a
    /// push from no authorised committer, and otherwise reads the changed content files at
    /// the commit, drafts and previews the change, and submits the owner's decision card.
    ///
    /// # Errors
    ///
    /// A source that cannot be read, the approval service's error, or the journal's; the
    /// kernel retries the run.
    pub async fn receive(
        &self,
        hook: &str,
        delivery: &str,
        payload: &Value,
    ) -> Result<PushOutcome> {
        let ignored = |reason: String| Ok(PushOutcome::Ignored { reason });
        let Some(hook) = self.config.hooks.iter().find(|h| h.name == hook) else {
            return ignored(format!("no hook `{hook}`"));
        };
        let Some(push) = PushEvent::parse(hook.forge, payload) else {
            return ignored("not a push".to_string());
        };
        if push.branch.as_deref() != Some(self.config.branch.as_str()) {
            return ignored(format!("not a push to {}", self.config.branch));
        }
        let Some(commit) = push.commit.clone() else {
            return ignored("the push deletes the branch".to_string());
        };
        // One push at a time, so a replay racing its original proposes nothing twice. A commit
        // an authorised committer pushed is taken once on any hook; one only signalled is
        // taken once on its own hook, and a listed committer may still push it.
        let mut records = self.records.lock().await;
        if records.iter().any(|r| {
            r.repository == push.repository
                && r.commit == commit
                && (r.committer.is_some() || r.hook == hook.name)
        }) {
            tracing::info!(website.repository.commit = %commit, "a replayed push is taken once");
            return Ok(PushOutcome::Duplicate { commit });
        }
        let mut record = PushRecord {
            hook: hook.name.clone(),
            delivery: delivery.to_string(),
            repository: push.repository.clone(),
            branch: self.config.branch.clone(),
            commit: commit.clone(),
            committer: None,
            at: Utc::now(),
            state: PushState::Ignored,
            card: None,
            items: Vec::new(),
            detail: None,
            release: None,
        };
        match self.config.committer(hook, &push) {
            None => {
                self.signal(&hook.name, &commit).await?;
                record.state = PushState::Signalled;
            }
            Some(committer) => {
                record.committer = Some(committer.id.clone());
                self.propose(&push, &commit, committer, &mut record).await?;
            }
        }
        records.push(record.clone());
        self.keep(&mut records).await?;
        Ok(PushOutcome::Taken(Box::new(record)))
    }

    /// The signal a push from no authorised committer files (spec 3.5 §7.10, A-I3, A-I24):
    /// typed fields only, no account name and no file.
    async fn signal(&self, hook: &str, commit: &str) -> Result<()> {
        tracing::info!(website.repository.hook = hook, website.repository.commit = %commit, "a push from no authorised committer becomes a signal");
        self.parts
            .signals
            .emit(Signal {
                id: uuid::Uuid::new_v4(),
                kind: SignalKind::InboundMessage {
                    channel: Channel::from("repository"),
                    thread_ref: ThreadRef(format!("{hook}:{}", &commit[..12])),
                    category: MessageCategory::Other,
                },
                emitted_by: format!("repository:{hook}"),
                emitted_at: Utc::now(),
            })
            .await
    }

    /// The content files a push changes: the source's own list when it has one.
    async fn changed(&self, push: &PushEvent, commit: &str) -> Result<Vec<FileChange>> {
        let listed = self
            .source
            .changes(&push.repository, push.before.as_deref(), commit)
            .await?
            .unwrap_or_else(|| push.files.clone());
        let prefix = format!("{CONTENT_DIR}/");
        Ok(listed
            .into_iter()
            .filter(|file| file.path.starts_with(&prefix) && file.path.ends_with(".md"))
            .collect())
    }

    /// The change a push makes: each changed content file read at the commit, each removed
    /// one withdrawn; `Err` with the reason for a file that is not a content item.
    async fn change(
        &self,
        push: &PushEvent,
        commit: &str,
        committer: &CommitterConfig,
    ) -> Result<std::result::Result<Change, String>> {
        let mut items: Vec<ContentItem> = Vec::new();
        for file in self.changed(push, commit).await? {
            let bytes = match file.removed {
                true => None,
                false => self.source.read(&push.repository, commit, &file.path).await?,
            };
            match bytes {
                Some(bytes) => {
                    let Ok(text) = String::from_utf8(bytes.to_vec()) else {
                        return Ok(Err(format!("{} is not UTF-8 text", file.path)));
                    };
                    match parse_content_file(&file.path, &text) {
                        Ok(item) => items.push(item),
                        Err(error) => return Ok(Err(error.to_string())),
                    }
                }
                None => {
                    let id = ContentId(
                        file.path
                            .trim_start_matches(&format!("{CONTENT_DIR}/"))
                            .trim_end_matches(".md")
                            .to_string(),
                    );
                    let store = self.parts.runtime.store();
                    if let Some(mut item) = store.get(&id).await.map_err(config)? {
                        item.fields.insert("withdrawal".into(), json!({ "mode": "gone" }));
                        item.state = ContentState::Withdrawn;
                        items.push(item);
                    }
                }
            }
        }
        Ok(Ok(Change {
            items,
            media: Vec::new(),
            entry: EntryPoint::Repository { committer: committer.id.clone() },
            proposer: committer.id.clone(),
            job: REPOSITORY_JOB.to_string(),
            action_class: "publish".to_string(),
        }))
    }

    /// Drafts, checks and previews an authorised committer's push, and cards it.
    async fn propose(
        &self,
        push: &PushEvent,
        commit: &str,
        committer: &CommitterConfig,
        record: &mut PushRecord,
    ) -> Result<()> {
        let change = match self.change(push, commit, committer).await? {
            Ok(change) if change.items.is_empty() => {
                record.detail = Some("no content file changed".to_string());
                return Ok(());
            }
            Ok(change) => change,
            Err(reason) => {
                record.state = PushState::Refused;
                record.detail = Some(reason);
                return Ok(());
            }
        };
        record.items = change.items.iter().map(|item| item.id.clone()).collect();
        let pipeline = self.parts.runtime.pipeline();
        let draft = match pipeline.draft(change).await {
            Ok(draft) => draft,
            Err(error) => {
                record.state = PushState::Refused;
                record.detail = Some(error.to_string());
                return Ok(());
            }
        };
        let preview = match pipeline.preview(&draft).await {
            Ok(preview) => preview,
            Err(error) => {
                record.state = PushState::Refused;
                record.detail = Some(error.to_string());
                return Ok(());
            }
        };
        if preview.gate.refuses_release() || !preview.gate.passed() {
            record.state =
                if preview.gate.refuses_release() { PushState::Refused } else { PushState::Held };
            record.detail = Some(preview.gate.summary());
            return Ok(());
        }
        let card = self.card(push, commit, committer, &draft, &preview).await?;
        tracing::info!(website.repository.commit = %commit, website.repository.card = %card, "a push waits for the owner's decision");
        record.state = PushState::Proposed;
        record.card = Some(card.clone());
        self.pending.lock().await.insert(card, Pending { draft, preview });
        Ok(())
    }

    /// Submits the owner's decision card for a previewed push, or finds the one submitted
    /// before a restart.
    async fn card(
        &self,
        push: &PushEvent,
        commit: &str,
        committer: &CommitterConfig,
        draft: &Draft,
        preview: &Preview,
    ) -> Result<String> {
        let id = format!("site-repository-{}-{}", self.parts.site.id, &commit[..12]);
        if self.parts.approvals.get(&id).await?.is_some() {
            return Ok(id);
        }
        let pages = preview.diff.len();
        let summary = format!(
            "{} pushed {} to {}: {} item{} and {pages} page{} change. Publish it or decline it.",
            committer.id,
            &commit[..7],
            self.config.branch,
            draft.ids.len(),
            if draft.ids.len() == 1 { "" } else { "s" },
            if pages == 1 { "" } else { "s" },
        );
        let args = json!({
            "repository": push.repository,
            "branch": self.config.branch,
            "commit": commit,
            "committer": committer.id,
            "items": draft.ids,
            "preview": preview.staged.digest,
            "diff": preview.diff,
        });
        let call = ToolConfirmationRequest::new(REPOSITORY_RELEASE, args);
        let owner = self.parts.owner.clone();
        let scope =
            ApprovalScope::new(self.parts.site.name.clone(), owner.clone(), "site-repository");
        let policy = ApprovalPolicy::new([ApproverSelector::human(owner)]);
        let request = ApprovalRequest::new(id.clone(), scope, &call, policy)
            .with_summary(summary)
            .with_risk(ApprovalRisk::Medium);
        self.parts.approvals.submit(request).await?;
        Ok(id)
    }

    /// Applies the owner's decided cards (spec 3.5 §7.8.2 step 4, A-I23): an approval records
    /// the owner's vote and releases once the approval chain is satisfied; a refusal or an
    /// expiry declines the push. Returns the records it changed.
    ///
    /// # Errors
    ///
    /// The approval service's, the source's or the journal's error.
    pub async fn step(&self) -> Result<Vec<PushRecord>> {
        let mut records = self.records.lock().await;
        let mut changed = Vec::new();
        for record in records.iter_mut() {
            if !matches!(record.state, PushState::Proposed | PushState::Approving) {
                continue;
            }
            let Some(card) = record.card.clone() else { continue };
            let Some(request) = self.parts.approvals.get(&card).await? else { continue };
            match request.state {
                ApprovalState::Pending | ApprovalState::Responded => continue,
                ApprovalState::Rejected | ApprovalState::Expired | ApprovalState::Cancelled => {
                    self.pending.lock().await.remove(&card);
                    record.state = PushState::Declined;
                    record.detail = request.decisions.last().and_then(|d| d.reason.clone());
                    changed.push(record.clone());
                }
                ApprovalState::Approved => {
                    let by = request
                        .decisions
                        .last()
                        .map_or_else(|| self.parts.owner.clone(), |d| d.approver.id.clone());
                    if self.release(record, &card, &by).await? {
                        changed.push(record.clone());
                    }
                }
            }
        }
        if !changed.is_empty() {
            self.keep(&mut records).await?;
        }
        Ok(changed)
    }

    /// Releases an approved push; `false` while its approval chain still waits.
    async fn release(&self, record: &mut PushRecord, card: &str, by: &str) -> Result<bool> {
        let mut pending = self.pending.lock().await;
        if !pending.contains_key(card) {
            // A restart forgot the draft: the commit is read and previewed again.
            let Some(rebuilt) = self.rebuild(record).await? else {
                record.state = PushState::Refused;
                record.detail = Some("the commit no longer previews".to_string());
                return Ok(true);
            };
            pending.insert(card.to_string(), rebuilt);
        }
        let Some(Pending { draft, preview }) = pending.get(card) else { return Ok(false) };
        let pipeline = self.parts.runtime.pipeline();
        let mut decided = pipeline.approve(draft, preview).await.map_err(config)?;
        if let (ApprovalDecision::Pending { ticket }, PushState::Proposed) =
            (&decided, &record.state)
        {
            // The owner's card is the owner's vote on the release's request under an approval
            // chain (A-I23); a vote that cannot count is left out.
            let approvals = self.parts.runtime.approvals();
            if let Some(request) = approvals.get(ticket).await? {
                match approvals.approve(ticket, request.version, &SubjectRef::human(by)).await {
                    Ok(_) => decided = pipeline.approve(draft, preview).await.map_err(config)?,
                    Err(error) => {
                        tracing::info!(website.release.request = %ticket, error = %error, "the owner's vote does not count for the chain");
                    }
                }
            }
        }
        let decision = match decided {
            ApprovalDecision::Pending { .. } => {
                let waiting = record.state != PushState::Approving;
                record.state = PushState::Approving;
                return Ok(waiting);
            }
            ApprovalDecision::Rejected { by, reason } => {
                pending.remove(card);
                record.state = PushState::Declined;
                record.detail = Some(format!("{by}: {reason}"));
                return Ok(true);
            }
            ApprovalDecision::Approved { decision, .. } => {
                decision.unwrap_or_else(|| format!("approval:{card}"))
            }
        };
        let Some(Pending { draft, preview }) = pending.remove(card) else { return Ok(false) };
        let outcome = pipeline.complete(&draft, Some(decision)).await;
        let _ = tokio::fs::remove_dir_all(&preview.staged.root).await;
        match outcome.map_err(config)? {
            PipelineOutcome::Published(published) => {
                record.state = PushState::Published;
                record.release = Some(published.staged.manifest.seq);
            }
            PipelineOutcome::Scheduled { .. } => record.state = PushState::Scheduled,
            PipelineOutcome::Held(report) => {
                record.state = PushState::Held;
                record.detail = Some(report.summary());
            }
            PipelineOutcome::Rejected { by, reason } => {
                record.state = PushState::Declined;
                record.detail = Some(format!("{by}: {reason}"));
            }
            PipelineOutcome::Pending { .. } => record.state = PushState::Approving,
        }
        tracing::info!(website.repository.commit = %record.commit, website.repository.state = ?record.state, "the owner's decision on a push is applied");
        Ok(true)
    }

    /// The draft and preview of a recorded push, read again from its commit.
    async fn rebuild(&self, record: &PushRecord) -> Result<Option<Pending>> {
        let Some(committer) =
            self.config.committers.iter().find(|c| Some(&c.id) == record.committer.as_ref())
        else {
            return Ok(None);
        };
        let push = PushEvent {
            repository: record.repository.clone(),
            branch: Some(record.branch.clone()),
            before: None,
            commit: Some(record.commit.clone()),
            pusher: None,
            files: record
                .items
                .iter()
                .map(|id| FileChange { path: format!("{CONTENT_DIR}/{}.md", id.0), removed: false })
                .collect(),
        };
        let Ok(change) = self.change(&push, &record.commit, committer).await? else {
            return Ok(None);
        };
        let pipeline = self.parts.runtime.pipeline();
        let draft = pipeline.draft(change).await.map_err(config)?;
        let preview = pipeline.preview(&draft).await.map_err(config)?;
        Ok(preview.gate.passed().then_some(Pending { draft, preview }))
    }

    /// Writes the journal, keeping the newest records.
    async fn keep(&self, records: &mut Vec<PushRecord>) -> Result<()> {
        if records.len() > KEPT_RECORDS {
            let excess = records.len() - KEPT_RECORDS;
            records.drain(..excess);
        }
        write_journal(&self.journal, records).await
    }

    /// Applies the owner's decisions every `idle` until aborted.
    pub fn spawn(self: Arc<Self>, idle: Duration) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                if let Err(error) = self.step().await {
                    tracing::warn!(error = %error, "the repository entry's step failed");
                }
                tokio::time::sleep(idle).await;
            }
        })
    }
}

async fn write_journal(path: &Path, records: &[PushRecord]) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(config)?;
    }
    let bytes = serde_json::to_vec_pretty(records).map_err(config)?;
    let temporary = path.with_extension("json.tmp");
    tokio::fs::write(&temporary, bytes).await.map_err(config)?;
    tokio::fs::rename(&temporary, path).await.map_err(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(c: char) -> String {
        c.to_string().repeat(40)
    }

    #[test]
    fn each_forge_names_its_pusher_and_the_last_change_of_each_file() {
        let gitlab = PushEvent::parse(
            Forge::Gitlab,
            &json!({
                "object_kind": "push", "ref": "refs/heads/main", "before": sha('1'), "after": sha('2'),
                "user_username": "wanjiru", "project": { "path_with_namespace": "news/site" },
                "commits": [
                    { "id": sha('3'), "added": ["content/a/x/en.md"], "modified": [], "removed": [] },
                    { "id": sha('2'), "added": [], "modified": [], "removed": ["content/a/x/en.md"] }
                ]
            }),
        )
        .unwrap();
        assert_eq!(gitlab.pusher.as_deref(), Some("wanjiru"));
        assert_eq!(gitlab.repository, "news/site");
        assert_eq!(gitlab.files, [FileChange { path: "content/a/x/en.md".into(), removed: true }]);
        let tag = PushEvent::parse(
            Forge::Gitlab,
            &json!({ "object_kind": "tag_push", "ref": "refs/tags/v1", "after": sha('2'), "commits": [] }),
        );
        assert!(tag.is_none());
        let signed = PushEvent::parse(
            Forge::Signed,
            &json!({ "repository": "studio/site", "ref": "refs/heads/main", "after": "0".repeat(40), "commits": [] }),
        )
        .unwrap();
        assert_eq!((signed.repository.as_str(), signed.commit), ("studio/site", None));
        assert_eq!(signed.pusher, None, "the signed form names no account: its key does");
    }

    #[test]
    fn committers_are_an_account_on_a_forge_hook_or_a_signed_hook_key() {
        let hook =
            |name: &str, forge| HookConfig { name: name.into(), forge, secret: "env:X".into() };
        let mut config = RepositoryConfig {
            branch: "main".into(),
            source: SourceConfig { git: Some(".".into()), ..SourceConfig::default() },
            hooks: vec![hook("github", Forge::Github), hook("studio", Forge::Signed)],
            committers: vec![
                CommitterConfig {
                    id: "wanjiru".into(),
                    account: Some("github:wanjiru-k".into()),
                    key: None,
                },
                CommitterConfig {
                    id: "newsdesk".into(),
                    account: None,
                    key: Some("studio".into()),
                },
            ],
        };
        config.validate().unwrap();
        let push = |pusher: Option<&str>| PushEvent {
            repository: "r".into(),
            branch: Some("main".into()),
            before: None,
            commit: Some(sha('a')),
            pusher: pusher.map(str::to_string),
            files: Vec::new(),
        };
        let github = &config.hooks[0];
        assert_eq!(config.committer(github, &push(Some("wanjiru-k"))).unwrap().id, "wanjiru");
        assert!(config.committer(github, &push(Some("mallory"))).is_none());
        assert!(config.committer(github, &push(None)).is_none());
        assert_eq!(
            config.committer(&config.hooks[1], &push(Some("mallory"))).unwrap().id,
            "newsdesk"
        );

        config.committers[1].key = Some("github".into());
        assert!(config.validate().is_err(), "a key is a signed hook");
        config.committers[1].key = None;
        assert!(config.validate().is_err(), "an account or a key");
        config.committers[1].account = Some("studio:newsdesk".into());
        assert!(config.validate().is_err(), "an account is on a forge's hook");
        config.committers.truncate(1);
        config.hooks.push(hook("github", Forge::Gitlab));
        assert!(config.validate().is_err(), "a hook is listed once");
        config.hooks.pop();
        config.source.http = Some("https://x/{commit}/{path}".into());
        assert!(config.validate().is_err(), "one source");
    }
}
