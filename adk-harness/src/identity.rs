//! Identity and credentials (spec 3.1 §7.14, §9.2 I5–I7, I39; feature `auth`).
//!
//! | Need | Item |
//! |---|---|
//! | Roles on a shared server | [`RbacMap`]: a harness tool class needs a permission, checked by `adk-auth`'s `ScopeGuard` (and `ProtectedTool` with an `AccessControl`) when the tool executes |
//! | Credentials for `git` and HTTPS tools | [`ShellCredential`]: tokens minted per call by a `CredentialBroker` for the subject the run acts for, handed to `bash` through a helper script that exists only for that call, revoked after it |
//! | Authenticated identity | `HarnessAgentBuilder::require_authenticated_identity`: on a served harness, standing rules and Auto apply only to a run whose user the server authenticated (I6) |
//! | Tamper-evident record | the signed `adk_auth::signed_audit::AuditLog`; [`EphemeralAuditLog`] when none is configured, so every run records in Block mode (I39) |

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use adk_auth::signed_audit::{AuditLog, Ed25519Signer, json_sha256};
use adk_auth::{AccessControl, ContextScopeResolver, ProtectedToolDyn, ScopeGuard, ScopeResolver};
use adk_core::audit_log::{AuditRecordDraft, AuditRecorder, InMemoryAuditLogStore};
use adk_core::{
    AdkError, CredentialBroker, ErrorCategory, ErrorComponent, ReadonlyContext, Result,
    RevocationScope, Tool, ToolContext, ToolEffect, Url,
};
use adk_devtools::{CommandLauncher, DevToolError, Workspace};
use async_trait::async_trait;
use serde_json::Value;

use crate::policy::ToolKind;

/// Error code of a harness run without an audit trail in Block mode (spec 3.1 §9.2 I39).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::HARNESS_AUDIT_REQUIRED_CODE, "harness.audit_required");
/// ```
pub const HARNESS_AUDIT_REQUIRED_CODE: &str = "harness.audit_required";

// ── RBAC ───────────────────────────────────────────────────────────────────────────

/// Permission names, interned once each, so a wrapper can return `&[&str]` from
/// `Tool::required_scopes`.
fn intern(permission: &str) -> &'static str {
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(name) = names.get(permission) {
        return name;
    }
    let name: &'static str = Box::leak(permission.to_string().into_boxed_str());
    names.insert(name);
    name
}

/// The permission each harness tool class requires (spec 3.1 §7.14).
///
/// The harness wraps every tool whose class the map names in `adk-auth`'s `ScopeGuard`,
/// with the permission as the required scope; with an
/// [`AccessControl`](RbacMap::with_access_control) it is also wrapped in `ProtectedTool`,
/// which checks the user's roles for the tool. The check runs when the tool executes,
/// after the governed path allowed the call, so a denial is a tool error naming the
/// missing permission. Scopes come from the run's authenticated request
/// (`ToolContext::user_scopes`) unless a [`ScopeResolver`] is set.
///
/// # Example
///
/// ```rust
/// use adk_harness::{Isolation, RbacMap, ToolKind};
///
/// let map = RbacMap::harness_defaults();
/// assert_eq!(map.permission_for(&ToolKind::Shell(Isolation::Host)), Some("harness:shell"));
/// assert_eq!(map.permission_for(&ToolKind::Payment), Some("harness:pay"));
/// assert_eq!(map.permission_for(&ToolKind::Read), None);
/// let custom = RbacMap::new().require(ToolKind::Edit, "repo:write");
/// assert_eq!(custom.permission_for(&ToolKind::Edit), Some("repo:write"));
/// ```
#[derive(Clone, Default)]
pub struct RbacMap {
    required: HashMap<ToolKind, &'static str>,
    resolver: Option<Arc<dyn ScopeResolver>>,
    access_control: Option<Arc<AccessControl>>,
}

impl std::fmt::Debug for RbacMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RbacMap")
            .field("required", &self.required)
            .field("resolver", &self.resolver.is_some())
            .field("access_control", &self.access_control.is_some())
            .finish()
    }
}

impl RbacMap {
    /// An empty map: no class needs a permission.
    pub fn new() -> Self {
        Self::default()
    }

    /// `harness:shell` for every shell class and the unsandboxed retry, `harness:edit` for
    /// edits, `harness:memory` for memory writes, `harness:pay` for payments.
    pub fn harness_defaults() -> Self {
        use crate::policy::Isolation;
        Self::new()
            .require(ToolKind::Shell(Isolation::Host), "harness:shell")
            .require(ToolKind::Shell(Isolation::OsSandbox), "harness:shell")
            .require(ToolKind::Shell(Isolation::Container), "harness:shell")
            .require(ToolKind::ShellEscalation, "harness:shell")
            .require(ToolKind::Edit, "harness:edit")
            .require(ToolKind::MemoryWrite, "harness:memory")
            .require(ToolKind::Payment, "harness:pay")
    }

    /// Requires `permission` for tools of `class`.
    #[must_use]
    pub fn require(mut self, class: ToolKind, permission: impl AsRef<str>) -> Self {
        self.required.insert(class, intern(permission.as_ref()));
        self
    }

    /// Resolves the caller's permissions with `resolver` instead of the run's
    /// authenticated scopes.
    #[must_use]
    pub fn with_scope_resolver(mut self, resolver: Arc<dyn ScopeResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Also checks the user's roles in `access_control` for each wrapped tool
    /// (`adk_auth::ProtectedTool`).
    #[must_use]
    pub fn with_access_control(mut self, access_control: Arc<AccessControl>) -> Self {
        self.access_control = Some(access_control);
        self
    }

    /// The permission a call of `class` needs.
    pub fn permission_for(&self, class: &ToolKind) -> Option<&'static str> {
        self.required.get(class).copied()
    }

    /// `tool` wrapped for `class`, or `tool` itself when the class needs no permission.
    pub(crate) fn protect(&self, tool: Arc<dyn Tool>, class: &ToolKind) -> Arc<dyn Tool> {
        let Some(permission) = self.permission_for(class) else {
            return tool;
        };
        let required: Arc<dyn Tool> =
            Arc::new(PermissionTool { inner: tool, role: PermissionRole::Declare([permission]) });
        let guard = match &self.resolver {
            Some(resolver) => ScopeGuard::new(SharedResolver(resolver.clone())),
            None => ScopeGuard::new(ContextScopeResolver),
        };
        let mut guarded =
            guard.protect_all(vec![required]).pop().expect("one tool in, one tool out");
        if let Some(access_control) = &self.access_control {
            guarded = Arc::new(ProtectedToolDyn::new(guarded, access_control.clone()));
        }
        let code = intern(&format!("harness.rbac.denied:{permission}"));
        Arc::new(PermissionTool { inner: guarded, role: PermissionRole::Explain { code } })
    }
}

struct SharedResolver(Arc<dyn ScopeResolver>);

#[async_trait]
impl ScopeResolver for SharedResolver {
    async fn resolve(&self, ctx: &dyn ToolContext) -> Vec<String> {
        self.0.resolve(ctx).await
    }
}

/// What a [`PermissionTool`] adds to the tool it wraps.
enum PermissionRole {
    /// Declares the permission as the tool's required scope, for `ScopeGuard`.
    Declare([&'static str; 1]),
    /// Turns a denial by the `adk-auth` wrappers inside into a `Forbidden` error whose code
    /// names the permission, which is the part of a tool error the model sees.
    Explain { code: &'static str },
}

/// A forwarding wrapper: every method is the wrapped tool's, so the declaration digest and
/// the tool's class are unchanged.
struct PermissionTool {
    inner: Arc<dyn Tool>,
    role: PermissionRole,
}

#[async_trait]
impl Tool for PermissionTool {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn description(&self) -> &str {
        self.inner.description()
    }
    fn declaration(&self) -> Value {
        self.inner.declaration()
    }
    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }
    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }
    fn is_builtin(&self) -> bool {
        self.inner.is_builtin()
    }
    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }
    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }
    fn annotations(&self) -> Option<&Value> {
        self.inner.annotations()
    }
    fn origin(&self) -> &str {
        self.inner.origin()
    }
    fn required_scopes(&self) -> &[&str] {
        match &self.role {
            PermissionRole::Declare(scopes) => scopes,
            PermissionRole::Explain { .. } => self.inner.required_scopes(),
        }
    }
    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }
    fn effect(&self) -> ToolEffect {
        self.inner.effect()
    }
    fn timeout_override(&self) -> Option<Option<std::time::Duration>> {
        self.inner.timeout_override()
    }
    fn is_concurrency_safe(&self) -> bool {
        self.inner.is_concurrency_safe()
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let result = self.inner.execute(ctx, args).await;
        match (&self.role, result) {
            (PermissionRole::Explain { code }, Err(error))
                if error.message.starts_with("missing required scopes")
                    || error.message.starts_with("Access denied: user") =>
            {
                Err(AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Forbidden,
                    code,
                    error.message,
                ))
            }
            (_, result) => result,
        }
    }
}

// ── Credentials ────────────────────────────────────────────────────────────────────

/// A resource `bash` gets a token for, through the credential helper (spec 3.1 §7.14).
///
/// # Example
///
/// ```rust
/// use adk_harness::ShellCredential;
///
/// let github = ShellCredential::new("https://github.com/").unwrap().with_scopes(["repo"]);
/// assert_eq!(github.resource.host_str(), Some("github.com"));
/// assert_eq!(github.username, "x-access-token");
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellCredential {
    /// The RFC 8707 resource the token is minted for; its host is what git asks about.
    pub resource: Url,
    /// Scopes requested for the token.
    pub scopes: Vec<String>,
    /// The user name git presents with the token.
    pub username: String,
}

impl ShellCredential {
    /// A credential for `resource` with no scopes and the user name `x-access-token`.
    ///
    /// # Errors
    ///
    /// `credential.resource.invalid` when `resource` is not an absolute HTTP(S) URI.
    pub fn new(resource: &str) -> Result<Self> {
        Ok(Self {
            resource: adk_core::resource_indicator(resource)?,
            scopes: Vec::new(),
            username: "x-access-token".to_string(),
        })
    }

    /// Requests `scopes`.
    #[must_use]
    pub fn with_scopes<I, S>(mut self, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.scopes = scopes.into_iter().map(Into::into).collect();
        self
    }

    /// Presents the token with `username`.
    #[must_use]
    pub fn with_username(mut self, username: impl Into<String>) -> Self {
        self.username = username.into();
        self
    }
}

/// The HTTPS remotes of the git repository at `root`, as credentials for their hosts.
pub(crate) fn git_https_remotes(root: &Path) -> Vec<ShellCredential> {
    let Ok(config) = std::fs::read_to_string(root.join(".git").join("config")) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    config
        .lines()
        .filter_map(|line| line.trim().strip_prefix("url"))
        .filter_map(|rest| rest.trim_start().strip_prefix('='))
        .filter_map(|url| Url::parse(url.trim()).ok())
        .filter(|url| url.scheme() == "https" && url.username().is_empty())
        .filter_map(|url| {
            let origin = format!("https://{}/", url.host_str()?);
            seen.insert(origin.clone()).then(|| ShellCredential::new(&origin).ok()).flatten()
        })
        .collect()
}

/// Environment of the calls that run with a credential helper, by function call id.
#[derive(Debug, Default)]
pub(crate) struct CallEnvironment {
    calls: Mutex<HashMap<String, Vec<(String, String)>>>,
}

impl CallEnvironment {
    fn set(&self, call_id: &str, env: Vec<(String, String)>) {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).insert(call_id.to_string(), env);
    }

    fn clear(&self, call_id: &str) {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).remove(call_id);
    }

    fn get(&self, call_id: &str) -> Vec<(String, String)> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(call_id)
            .cloned()
            .unwrap_or_default()
    }
}

/// A [`CommandLauncher`] that adds the credential helper's environment to the command of
/// a call that has one (spec 3.1 §7.14).
pub(crate) struct CredentialLauncher {
    inner: Arc<dyn CommandLauncher>,
    calls: Arc<CallEnvironment>,
}

impl CredentialLauncher {
    pub(crate) fn new(inner: Arc<dyn CommandLauncher>, calls: Arc<CallEnvironment>) -> Self {
        Self { inner, calls }
    }
}

impl CommandLauncher for CredentialLauncher {
    fn command(
        &self,
        ws: &Workspace,
        script: &str,
        ctx: &dyn ToolContext,
    ) -> std::result::Result<tokio::process::Command, DevToolError> {
        let mut command = self.inner.command(ws, script, ctx)?;
        for (key, value) in self.calls.get(ctx.function_call_id()) {
            command.env(key, value);
        }
        Ok(command)
    }

    fn isolation(&self) -> &str {
        self.inner.isolation()
    }
}

/// Shell single-quoting.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The helper script: a git credential helper (`get`) and a `GIT_ASKPASS` program in one.
fn helper_script(tokens: &[(String, String, String)]) -> String {
    let mut get = String::new();
    let mut ask = String::new();
    for (host, username, secret) in tokens {
        get.push_str(&format!(
            "      {}) printf 'username=%s\\npassword=%s\\n' {} {} ;;\n",
            quote(host),
            quote(username),
            quote(secret)
        ));
        let pattern = host.replace('*', "");
        ask.push_str(&format!(
            "      Username*{pattern}*) printf '%s\\n' {} ;;\n      Password*{pattern}*) printf '%s\\n' {} ;;\n",
            quote(username),
            quote(secret)
        ));
    }
    format!(
        "#!/bin/sh\n# Credential helper for one harness bash call; deleted when the call ends.\ncase \"$1\" in\n  get)\n    host=''\n    while IFS='=' read -r key value; do\n      [ -z \"$key\" ] && break\n      [ \"$key\" = host ] && host=\"$value\"\n    done\n    case \"$host\" in\n{get}    esac ;;\n  store|erase) ;;\n  *)\n    case \"$1\" in\n{ask}    esac ;;\nesac\n"
    )
}

/// Mints subject-scoped tokens around each call of the wrapped tool and hands them to
/// the shell through a helper script that exists only for that call (spec 3.1 §7.14).
///
/// Tokens never enter the harness process's environment or the shell's: the shell gets
/// `GIT_ASKPASS` and a git credential helper pointing at the script, which prints the
/// token for the host git asks about. After the call the script is deleted and the
/// tokens are revoked.
pub(crate) struct CredentialedTool {
    inner: Arc<dyn Tool>,
    broker: Arc<dyn CredentialBroker>,
    credentials: Vec<ShellCredential>,
    calls: Arc<CallEnvironment>,
    helper_dir: PathBuf,
}

impl CredentialedTool {
    pub(crate) fn new(
        inner: Arc<dyn Tool>,
        broker: Arc<dyn CredentialBroker>,
        credentials: Vec<ShellCredential>,
        calls: Arc<CallEnvironment>,
    ) -> Self {
        Self { inner, broker, credentials, calls, helper_dir: std::env::temp_dir() }
    }

    async fn mint(&self, ctx: &dyn ToolContext) -> Vec<(ShellCredential, String)> {
        let acting = ctx.acting_context();
        let mut tokens = Vec::new();
        for credential in &self.credentials {
            match self.broker.token_for(&acting, &credential.resource, &credential.scopes).await {
                Ok(token) => match token.authorize_for(&credential.resource) {
                    Ok(secret) => tokens.push((credential.clone(), secret.expose().to_string())),
                    Err(error) => {
                        tracing::warn!(error = %error, credential.resource = %credential.resource, "the broker's token does not cover the resource")
                    }
                },
                Err(error) => tracing::warn!(
                    error = %error,
                    credential.resource = %credential.resource,
                    "no token for the shell; the command runs without one"
                ),
            }
        }
        tokens
    }

    async fn revoke(&self, ctx: &dyn ToolContext, minted: &[(ShellCredential, String)]) {
        let agent = ctx.acting_context().agent.id;
        for (credential, _) in minted {
            let scope = RevocationScope::AgentResource {
                agent: agent.clone(),
                resource: credential.resource.clone(),
            };
            if let Err(error) = self.broker.revoke(&scope).await {
                tracing::warn!(error = %error, credential.resource = %credential.resource, "could not revoke the shell's token");
            }
        }
    }

    fn write_helper(&self, dir: &Path, minted: &[(ShellCredential, String)]) -> Result<PathBuf> {
        let io = |error: std::io::Error| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Unavailable,
                "harness.credentials.io",
                format!("write the credential helper: {error}"),
            )
        };
        std::fs::create_dir(dir).map_err(io)?;
        let path = dir.join("credential-helper");
        let tokens: Vec<(String, String, String)> = minted
            .iter()
            .filter_map(|(credential, secret)| {
                let host = match credential.resource.port() {
                    Some(port) => format!("{}:{port}", credential.resource.host_str()?),
                    None => credential.resource.host_str()?.to_string(),
                };
                Some((host, credential.username.clone(), secret.clone()))
            })
            .collect();
        #[cfg(unix)]
        {
            use std::io::Write as _;
            use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(io)?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .open(&path)
                .map_err(io)?;
            file.write_all(helper_script(&tokens).as_bytes()).map_err(io)?;
        }
        #[cfg(not(unix))]
        std::fs::write(&path, helper_script(&tokens)).map_err(io)?;
        Ok(path)
    }
}

#[async_trait]
impl Tool for CredentialedTool {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn description(&self) -> &str {
        self.inner.description()
    }
    fn declaration(&self) -> Value {
        self.inner.declaration()
    }
    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }
    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }
    fn is_builtin(&self) -> bool {
        self.inner.is_builtin()
    }
    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }
    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }
    fn annotations(&self) -> Option<&Value> {
        self.inner.annotations()
    }
    fn origin(&self) -> &str {
        self.inner.origin()
    }
    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }
    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }
    fn effect(&self) -> ToolEffect {
        self.inner.effect()
    }
    fn timeout_override(&self) -> Option<Option<std::time::Duration>> {
        self.inner.timeout_override()
    }
    fn is_concurrency_safe(&self) -> bool {
        self.inner.is_concurrency_safe()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let minted = self.mint(ctx.as_ref()).await;
        if minted.is_empty() {
            return self.inner.execute(ctx, args).await;
        }
        let call_id = ctx.function_call_id().to_string();
        let dir = self
            .helper_dir
            .join(format!("adk-harness-credentials-{}", uuid::Uuid::new_v4().simple()));
        let result = match self.write_helper(&dir, &minted) {
            Ok(helper) => {
                let helper = helper.to_string_lossy().into_owned();
                self.calls.set(
                    &call_id,
                    vec![
                        ("GIT_ASKPASS".to_string(), helper.clone()),
                        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
                        ("GIT_CONFIG_COUNT".to_string(), "2".to_string()),
                        ("GIT_CONFIG_KEY_0".to_string(), "credential.helper".to_string()),
                        ("GIT_CONFIG_VALUE_0".to_string(), String::new()),
                        ("GIT_CONFIG_KEY_1".to_string(), "credential.helper".to_string()),
                        ("GIT_CONFIG_VALUE_1".to_string(), format!("!{}", quote(&helper))),
                        ("ADK_CREDENTIAL_HELPER".to_string(), helper),
                    ],
                );
                let result = self.inner.execute(ctx.clone(), args).await;
                self.calls.clear(&call_id);
                result
            }
            Err(error) => Err(error),
        };
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(error = %error, "could not delete the credential helper");
        }
        self.revoke(ctx.as_ref(), &minted).await;
        result
    }
}

// ── Authenticated identity (I6) ────────────────────────────────────────────────────

/// Which invocations ran for a user the server authenticated, for the policy
/// (spec 3.1 §9.2 I6).
#[derive(Debug, Default)]
pub(crate) struct IdentityGate {
    require_authenticated: bool,
    seen: Mutex<HashMap<String, bool>>,
}

impl IdentityGate {
    pub(crate) fn new(require_authenticated: bool) -> Self {
        Self { require_authenticated, seen: Mutex::new(HashMap::new()) }
    }

    /// Records whether `ctx`'s invocation is authenticated.
    pub(crate) fn observe(&self, ctx: &dyn ReadonlyContext) {
        if !self.require_authenticated {
            return;
        }
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if seen.len() > 8192 {
            seen.clear();
        }
        seen.insert(ctx.invocation_id().to_string(), ctx.is_authenticated());
    }

    /// Whether standing rules and Auto are inert for `invocation_id`: the harness requires
    /// authenticated identity and the invocation was not authenticated, or was never seen.
    pub(crate) fn inert(&self, invocation_id: &str) -> bool {
        self.require_authenticated
            && !self
                .seen
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(invocation_id)
                .copied()
                .unwrap_or(false)
    }
}

#[async_trait]
impl crate::toolset::RequestObserver for IdentityGate {
    async fn before_request(
        &self,
        ctx: &Arc<dyn ReadonlyContext>,
        _request: &mut adk_core::LlmRequest,
    ) -> Result<()> {
        self.observe(ctx.as_ref());
        Ok(())
    }
}

// ── Signed audit (I39) ─────────────────────────────────────────────────────────────

/// A signed, Merkle-hashed `AuditLog` kept in memory with a key generated at start-up:
/// the harness's audit log when none is configured (spec 3.1 §9.2 I39).
///
/// Records are lost when the process exits and the key is never persisted, so a
/// deployment passes its own `AuditLog` over a durable `AuditLogStore` to
/// `HarnessAgentBuilder::audit_log`.
///
/// # Example
///
/// ```rust
/// use adk_core::audit_log::{AuditRecordDraft, AuditRecordKind, AuditRecorder};
/// use adk_harness::EphemeralAuditLog;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let log = EphemeralAuditLog::new().unwrap();
/// log.record(AuditRecordDraft::new(AuditRecordKind::ToolCall, "search")).await.unwrap();
/// assert!(log.verify().await.unwrap());
/// # });
/// ```
pub struct EphemeralAuditLog {
    store: Arc<InMemoryAuditLogStore>,
    signer: Arc<Ed25519Signer>,
    log: tokio::sync::OnceCell<AuditLog>,
}

impl std::fmt::Debug for EphemeralAuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EphemeralAuditLog").finish_non_exhaustive()
    }
}

/// Log id of an [`EphemeralAuditLog`].
const EPHEMERAL_LOG_ID: &str = "adk-harness";

impl EphemeralAuditLog {
    /// A new log with a freshly generated Ed25519 key.
    ///
    /// # Errors
    ///
    /// Returns an error when the system random number generator fails.
    pub fn new() -> Result<Self> {
        let signer = Ed25519Signer::generate("adk-harness.local").map_err(|error| {
            AdkError::config(format!("generate the harness audit signing key: {error}"))
        })?;
        Ok(Self {
            store: Arc::new(InMemoryAuditLogStore::new()),
            signer: Arc::new(signer),
            log: tokio::sync::OnceCell::new(),
        })
    }

    async fn log(&self) -> Result<&AuditLog> {
        self.log
            .get_or_try_init(|| async {
                AuditLog::builder(self.store.clone(), EPHEMERAL_LOG_ID, self.signer.clone())
                    .open()
                    .await
            })
            .await
    }

    /// The store the records are in.
    pub fn store(&self) -> &Arc<InMemoryAuditLogStore> {
        &self.store
    }

    /// The signed-note verifier key of the log's checkpoints.
    pub fn verifier_key(&self) -> String {
        use adk_auth::signed_audit::AuditSigner as _;
        self.signer.verifier_key()
    }

    /// Signs a checkpoint and verifies the whole log against it: hash chain, Merkle tree
    /// and signature.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn verify(&self) -> Result<bool> {
        let log = self.log().await?;
        log.checkpoint().await?;
        let export =
            adk_auth::signed_audit::AuditExport::from_store(self.store.as_ref(), log.log_id())
                .await?;
        let trusted = self
            .verifier_key()
            .parse::<adk_auth::signed_audit::NoteVerifier>()
            .map_err(|error| AdkError::config(format!("the log's verifier key: {error}")))?;
        Ok(export.verify(&[trusted]).is_valid())
    }
}

#[async_trait]
impl AuditRecorder for EphemeralAuditLog {
    fn digest(&self, value: &Value) -> String {
        json_sha256(value).unwrap_or_else(|_| {
            format!(
                "sha256-json:{}",
                crate::fsutil::hex_digest(
                    serde_json::to_string(value).unwrap_or_default().as_bytes()
                )
            )
        })
    }

    async fn record(&self, draft: AuditRecordDraft) -> Result<()> {
        self.log().await?.append(draft).await.map(drop)
    }
}

/// The refusal of a run whose audit trail is missing or does not block (spec 3.1 §9.2 I39).
pub(crate) fn audit_required(agent: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Forbidden,
        HARNESS_AUDIT_REQUIRED_CODE,
        format!(
            "harness '{agent}' runs only with an audit trail in Block mode (RunConfig::audit): \
             set one with Runner::builder().audit_log(..) and the default \
             AuditFailureMode::Block, or HarnessAgentBuilder::audit_log(..)"
        ),
    )
}

/// The credential resources of a harness: the configured ones, else the workspace's HTTPS
/// git remotes.
pub(crate) fn resolve_credentials(
    configured: Vec<ShellCredential>,
    root: &Path,
) -> Vec<ShellCredential> {
    if !configured.is_empty() {
        return configured;
    }
    let remotes = git_https_remotes(root);
    if remotes.is_empty() {
        tracing::warn!(
            "a credential broker is set but no shell credential is configured and the workspace \
             has no HTTPS git remote; bash runs without tokens"
        );
    }
    remotes
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::ToolOrigin;
    use serde_json::json;

    /// A tool with its own origin, annotations and declaration, as an MCP tool has.
    #[derive(Debug)]
    struct Declared;

    #[async_trait]
    impl Tool for Declared {
        fn name(&self) -> &str {
            "read"
        }

        fn description(&self) -> &str {
            "reads"
        }

        fn declaration(&self) -> Value {
            json!({
                "name": "read",
                "description": "reads",
                "parameters": { "type": "object", "properties": { "path": { "type": "string" } } },
                "x-server": "fs"
            })
        }

        fn annotations(&self) -> Option<&Value> {
            static HINTS: OnceLock<Value> = OnceLock::new();
            Some(HINTS.get_or_init(|| json!({ "readOnlyHint": true })))
        }

        fn origin(&self) -> &str {
            "mcp:fs"
        }

        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }

        async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
            Ok(Value::Null)
        }
    }

    /// A broker that mints nothing.
    #[derive(Debug)]
    struct NoBroker;

    #[async_trait]
    impl CredentialBroker for NoBroker {
        async fn token_for(
            &self,
            _acting: &adk_core::ActingContext,
            resource: &Url,
            _scopes: &[String],
        ) -> Result<adk_core::ScopedToken> {
            Err(AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Unavailable,
                "test.broker.none",
                format!("no credential for {resource}"),
            ))
        }

        async fn revoke(&self, _scope: &RevocationScope) -> Result<u64> {
            Ok(0)
        }
    }

    #[test]
    fn the_wrappers_keep_the_wrapped_tools_origin_and_declaration_digest() {
        let inner: Arc<dyn Tool> = Arc::new(Declared);
        let expected = ToolOrigin::of(inner.as_ref());
        assert_eq!(expected.source, "mcp:fs");

        let permission = PermissionTool {
            inner: inner.clone(),
            role: PermissionRole::Explain { code: "harness.rbac.denied:harness:shell" },
        };
        assert_eq!(ToolOrigin::of(&permission), expected, "PermissionTool");
        assert_eq!(permission.annotations(), inner.annotations());

        let credentialed = CredentialedTool::new(
            inner.clone(),
            Arc::new(NoBroker),
            Vec::new(),
            Arc::new(CallEnvironment::default()),
        );
        assert_eq!(ToolOrigin::of(&credentialed), expected, "CredentialedTool");
        assert_eq!(credentialed.effect(), ToolEffect::ReadOnly);
    }

    #[test]
    fn https_remotes_become_credentials_once_per_host() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(".git/config"),
            "[remote \"origin\"]\n\turl = https://github.com/acme/app.git\n[remote \"fork\"]\n\turl = https://github.com/me/app.git\n[remote \"ssh\"]\n\turl = git@gitlab.com:acme/app.git\n[remote \"x\"]\n\turl = https://user@example.com/x.git\n",
        )
        .unwrap();
        let credentials = git_https_remotes(dir.path());
        assert_eq!(credentials, vec![ShellCredential::new("https://github.com/").unwrap()]);
    }

    #[test]
    fn the_helper_quotes_secrets() {
        let script = helper_script(&[("github.com".into(), "x-access-token".into(), "a'b".into())]);
        assert!(script.contains("'a'\\''b'"), "{script}");
        assert!(script.starts_with("#!/bin/sh\n"));
    }

    #[test]
    fn the_gate_is_inert_only_when_identity_is_required() {
        let open = IdentityGate::new(false);
        assert!(!open.inert("inv"));
        let served = IdentityGate::new(true);
        assert!(served.inert("never-seen"));
    }
}
