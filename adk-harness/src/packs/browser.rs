//! The browser pack (spec 3.1 §7.24, feature `browser`).
//!
//! `adk-browser`'s `BrowserToolset` does the work; the pack is classes and wiring. Every
//! browser tool name is registered at build: observation tools as [`ToolKind::Read`], the
//! navigation tools as [`ToolKind::Web`] decided by the URL's host under the §7.20 rules,
//! and every action that changes page or browser state as `External` — denied in ReadOnly
//! and Plan, held for approval in Ask and Auto (§9.2 I58, I60). The pack never attaches
//! `browser_evaluate_js` or `browser_file_upload`: model-written code in the page and host
//! files handed to a page have no governed form, as I27 refuses provider shells.
//!
//! Each tool is wrapped so that a navigation passes the composed-URL checks of §7.20
//! before the browser moves (I48), text typed into a page is refused when it matches a
//! secret pattern, and text the browser returns is screened and delivered inside a
//! delimited untrusted block, as fetched text is (I47).

use std::sync::Arc;

use adk_browser::{BrowserConfig, BrowserProfile, BrowserSession, BrowserToolset};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::audit::HarnessAudit;
use crate::fsutil::hex_digest;
use crate::policy::ToolKind;
use crate::web::{Fetcher, WebConfig, has_secret, host_allowlisted, screen, untrusted_block};

/// Observation tools, class `Read` (spec 3.1 §7.24).
///
/// # Example
///
/// ```rust
/// assert!(adk_harness::packs::browser::BROWSER_READ_TOOLS.contains(&"browser_extract_text"));
/// ```
pub const BROWSER_READ_TOOLS: &[&str] = &[
    "browser_extract_text",
    "browser_extract_attribute",
    "browser_extract_links",
    "browser_page_info",
    "browser_page_source",
    "browser_screenshot",
    "browser_element_state",
    "browser_wait",
    "browser_wait_for_element",
    "browser_wait_for_page_load",
    "browser_wait_for_text",
    "browser_list_windows",
    "browser_scroll",
    "browser_switch_to_frame",
    "browser_switch_to_parent_frame",
    "browser_switch_to_default_content",
    "browser_switch_window",
    "browser_print_to_pdf",
];

/// Navigation tools, class `Web { allowlisted }` from their `url` (spec 3.1 §7.24).
///
/// # Example
///
/// ```rust
/// assert!(adk_harness::packs::browser::BROWSER_NAVIGATION_TOOLS.contains(&"browser_navigate"));
/// ```
pub const BROWSER_NAVIGATION_TOOLS: &[&str] =
    &["browser_navigate", "browser_new_tab", "browser_new_window"];

/// History tools, class `Web { allowlisted: false }`: the destination is unknown until the
/// page loads (spec 3.1 §7.24).
///
/// # Example
///
/// ```rust
/// assert!(adk_harness::packs::browser::BROWSER_HISTORY_TOOLS.contains(&"browser_back"));
/// ```
pub const BROWSER_HISTORY_TOOLS: &[&str] = &["browser_back", "browser_forward", "browser_refresh"];

/// Actions that change page or browser state, class `External { NonIdempotent }`
/// (spec 3.1 §7.24).
///
/// # Example
///
/// ```rust
/// assert!(adk_harness::packs::browser::BROWSER_ACTION_TOOLS.contains(&"browser_click"));
/// ```
pub const BROWSER_ACTION_TOOLS: &[&str] = &[
    "browser_click",
    "browser_double_click",
    "browser_right_click",
    "browser_type",
    "browser_clear",
    "browser_select",
    "browser_focus",
    "browser_press_key",
    "browser_hover",
    "browser_drag_and_drop",
    "browser_handle_alert",
    "browser_close_window",
    "browser_maximize_window",
    "browser_minimize_window",
    "browser_set_window_size",
    "browser_get_cookies",
    "browser_get_cookie",
    "browser_add_cookie",
    "browser_delete_cookie",
    "browser_delete_all_cookies",
];

/// Tools the pack never attaches: model-written code in the page and host files handed to
/// a page have no governed form (spec 3.1 §7.24, §9.2 I58).
///
/// # Example
///
/// ```rust
/// assert!(adk_harness::packs::browser::BROWSER_NEVER_ATTACHED.contains(&"browser_evaluate_js"));
/// ```
pub const BROWSER_NEVER_ATTACHED: &[&str] = &["browser_evaluate_js", "browser_file_upload"];

/// Action of the record one navigation writes, write-ahead and after the page loaded
/// (spec 3.1 §9.3).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::packs::browser::BROWSER_AUDIT_NAVIGATED, "harness.browser.navigated");
/// ```
pub const BROWSER_AUDIT_NAVIGATED: &str = "harness.browser.navigated";

/// Action of the record one browser action writes (spec 3.1 §9.3).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::packs::browser::BROWSER_AUDIT_ACTED, "harness.browser.acted");
/// ```
pub const BROWSER_AUDIT_ACTED: &str = "harness.browser.acted";

/// Error code of text refused because it matches a secret pattern (spec 3.1 §7.24, I58).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::packs::browser::BROWSER_SECRET_IN_INPUT_CODE, "harness.browser.secret_in_input");
/// ```
pub const BROWSER_SECRET_IN_INPUT_CODE: &str = "harness.browser.secret_in_input";

/// Whether `name` is a tool the pack registers, attached or not.
///
/// # Example
///
/// ```rust
/// use adk_harness::packs::browser::is_browser_tool;
///
/// assert!(is_browser_tool("browser_navigate"));
/// assert!(is_browser_tool("browser_evaluate_js"));
/// assert!(!is_browser_tool("web_fetch"));
/// ```
pub fn is_browser_tool(name: &str) -> bool {
    browser_class(name).is_some() || BROWSER_NEVER_ATTACHED.contains(&name)
}

/// The class of a browser tool (spec 3.1 §7.24), or `None` for a name the pack does not
/// know. A `Web` class is decided per call from the URL's host, so the registry holds
/// `allowlisted: false`.
///
/// # Example
///
/// ```rust
/// use adk_core::ToolEffect;
/// use adk_harness::ToolKind;
/// use adk_harness::packs::browser::browser_class;
///
/// assert_eq!(browser_class("browser_extract_text"), Some(ToolKind::Read));
/// assert_eq!(browser_class("browser_navigate"), Some(ToolKind::Web { allowlisted: false }));
/// assert_eq!(
///     browser_class("browser_click"),
///     Some(ToolKind::External { effect: ToolEffect::NonIdempotent, trusted_origin: false })
/// );
/// assert_eq!(browser_class("browser_evaluate_js"), None);
/// ```
pub fn browser_class(name: &str) -> Option<ToolKind> {
    if BROWSER_READ_TOOLS.contains(&name) {
        Some(ToolKind::Read)
    } else if BROWSER_NAVIGATION_TOOLS.contains(&name) || BROWSER_HISTORY_TOOLS.contains(&name) {
        Some(ToolKind::Web { allowlisted: false })
    } else if BROWSER_ACTION_TOOLS.contains(&name) {
        Some(ToolKind::External { effect: ToolEffect::NonIdempotent, trusted_origin: false })
    } else {
        None
    }
}

/// Whether a team member's toolset drops `name`: every navigation and action; a member
/// keeps the observation tools (spec 3.1 §7.24 "Members", §9.2 I60).
///
/// # Example
///
/// ```rust
/// use adk_harness::packs::browser::browser_member_excluded;
///
/// assert!(browser_member_excluded("browser_click"));
/// assert!(browser_member_excluded("browser_navigate"));
/// assert!(!browser_member_excluded("browser_extract_text"));
/// ```
pub fn browser_member_excluded(name: &str) -> bool {
    BROWSER_NAVIGATION_TOOLS.contains(&name)
        || BROWSER_HISTORY_TOOLS.contains(&name)
        || BROWSER_ACTION_TOOLS.contains(&name)
}

/// Every registered browser tool with its class, for the class registry.
pub(crate) fn browser_classes() -> impl Iterator<Item = (&'static str, ToolKind)> {
    BROWSER_READ_TOOLS
        .iter()
        .chain(BROWSER_NAVIGATION_TOOLS)
        .chain(BROWSER_HISTORY_TOOLS)
        .chain(BROWSER_ACTION_TOOLS)
        .filter_map(|name| browser_class(name).map(|kind| (*name, kind)))
}

/// The browser pack (spec 3.1 §7.24): `adk-browser`'s tools under the harness policy.
///
/// `HarnessAgentBuilder::pack_browser` installs it. The toolset is
/// `BrowserToolset::with_profile` over the pack's session — `FormFilling` by default —
/// without `browser_evaluate_js` or `browser_file_upload`. The navigation allowlist is the
/// harness's `WebConfig::allowed_domains`.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_browser::{BrowserConfig, BrowserSession};
/// use adk_harness::{HarnessAgent, WebConfig, packs::browser::BrowserPack};
///
/// let session = Arc::new(BrowserSession::new(BrowserConfig::default()));
/// let harness = HarnessAgent::builder("harness")
///     .model(model)
///     .workspace(workspace)
///     .web(WebConfig { allowed_domains: vec!["docs.rs".into()], ..WebConfig::default() })
///     .pack_browser(BrowserPack::new(session))
///     .build()?;
/// assert_eq!(harness.info().packs, vec!["browser".to_string()]);
/// ```
pub struct BrowserPack {
    session: Arc<BrowserSession>,
    profile: BrowserProfile,
    allow_private_network: bool,
}

impl std::fmt::Debug for BrowserPack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserPack")
            .field("profile", &self.profile)
            .field("allow_private_network", &self.allow_private_network)
            .finish_non_exhaustive()
    }
}

impl Default for BrowserPack {
    /// A pack over `BrowserConfig::default()`: Chrome, headless, WebDriver at
    /// `http://localhost:4444`, started on first use. `Pack::Browser` installs this.
    fn default() -> Self {
        Self::new(Arc::new(BrowserSession::new(BrowserConfig::default())))
    }
}

impl BrowserPack {
    /// A pack over `session`, with the `FormFilling` profile.
    pub fn new(session: Arc<BrowserSession>) -> Self {
        Self { session, profile: BrowserProfile::FormFilling, allow_private_network: false }
    }

    /// The tool profile (`BrowserProfile::Full` for every tool the pack can attach).
    #[must_use]
    pub fn with_profile(mut self, profile: BrowserProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Lets the browser open loopback, private and link-local addresses: for test fixtures
    /// only, paired with `adk_harness::web::fixture::permit_loopback` so the policy admits
    /// them too.
    #[must_use]
    pub fn with_private_network_access(mut self, enabled: bool) -> Self {
        self.allow_private_network = enabled;
        self
    }

    /// The browser session the tools drive.
    pub fn session(&self) -> &Arc<BrowserSession> {
        &self.session
    }

    /// The governed tools, under the harness's web configuration.
    pub(crate) fn into_runtime(self, web: WebConfig) -> BrowserRuntime {
        let shared = Arc::new(Shared { fetcher: Fetcher::new(web.clone()), web });
        let toolset = BrowserToolset::with_profile(self.session.clone(), self.profile)
            .with_private_network_access(self.allow_private_network);
        let tools = toolset
            .all_tools()
            .into_iter()
            .filter(|tool| !BROWSER_NEVER_ATTACHED.contains(&tool.name()))
            .filter_map(|tool| {
                let role = Role::of(tool.name())?;
                Some(Arc::new(GovernedBrowserTool { inner: tool, role, shared: shared.clone() })
                    as Arc<dyn Tool>)
            })
            .collect();
        BrowserRuntime { tools, session: self.session }
    }
}

/// What the builder installs for a configured pack.
pub(crate) struct BrowserRuntime {
    pub(crate) tools: Vec<Arc<dyn Tool>>,
    #[allow(dead_code)]
    pub(crate) session: Arc<BrowserSession>,
}

struct Shared {
    web: WebConfig,
    fetcher: Fetcher,
}

/// How the wrapper treats a tool (spec 3.1 §7.24).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// Observation: the result is screened and delivered as untrusted text.
    Read,
    /// Navigation by `url`: the composed-URL checks before the browser moves, audited.
    Navigation,
    /// History: audited as a navigation whose destination is known only afterwards.
    History,
    /// An action: typed text screened, audited.
    Action,
}

impl Role {
    fn of(name: &str) -> Option<Self> {
        if BROWSER_READ_TOOLS.contains(&name) {
            Some(Self::Read)
        } else if BROWSER_NAVIGATION_TOOLS.contains(&name) {
            Some(Self::Navigation)
        } else if BROWSER_HISTORY_TOOLS.contains(&name) {
            Some(Self::History)
        } else if BROWSER_ACTION_TOOLS.contains(&name) {
            Some(Self::Action)
        } else {
            None
        }
    }
}

/// Tools whose result is bytes, delivered as the tool returns it.
const BINARY_RESULT_TOOLS: &[&str] = &["browser_screenshot", "browser_print_to_pdf"];

fn refused(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Tool, ErrorCategory::Forbidden, code, message)
}

/// A browser tool under the pack's rules. It forwards every declaration method, so the
/// tool's origin — and every held call and standing rule bound to it — is unchanged.
struct GovernedBrowserTool {
    inner: Arc<dyn Tool>,
    role: Role,
    shared: Arc<Shared>,
}

impl GovernedBrowserTool {
    /// Screens every string the result holds and delivers the result inside the untrusted
    /// block (spec 3.1 §7.24 "Delivery", §9.2 I47).
    async fn deliver(&self, ctx: &dyn ToolContext, result: Value) -> Result<Value> {
        if BINARY_RESULT_TOOLS.contains(&self.inner.name()) {
            return Ok(result);
        }
        // The page's URL: the navigation tools' `url`, the action tools' `page.url`.
        let url = result
            .get("url")
            .or_else(|| result.pointer("/page/url"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let rendered = serde_json::to_string_pretty(&result).unwrap_or_default();
        let screened = screen(&rendered).await;
        let flagged = screened.flagged();
        let sha256 = hex_digest(screened.text.as_bytes());
        if flagged && let Some(audit) = HarnessAudit::for_tool(ctx) {
            audit
                .record(
                    "harness.web.screened",
                    json!({
                        "tool": self.inner.name(),
                        "url": url,
                        "contentSha256": sha256,
                        "patterns": screened.patterns,
                        "lines": screened.injection_lines,
                        "redactions": screened.redactions,
                        "action": screened.action(),
                    }),
                )
                .await?;
        }
        let mut attributes = vec![("tool", self.inner.name().to_string())];
        if let Some(url) = &url {
            attributes.push(("url", url.clone()));
        }
        attributes.push(("sha256", sha256.clone()));
        let attributes: Vec<(&str, &str)> =
            attributes.iter().map(|(name, value)| (*name, value.as_str())).collect();
        Ok(json!({
            "url": url,
            "sha256": sha256,
            "screened": flagged,
            "content": untrusted_block("browser", &attributes, &screened.text),
        }))
    }

    async fn navigate(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let audit = HarnessAudit::for_tool(ctx.as_ref());
        let call_id = ctx.function_call_id().to_string();
        let url = args.get("url").and_then(Value::as_str).unwrap_or_default().to_string();
        let url_digest = hex_digest(url.as_bytes());
        // The composed-URL checks of §7.20 before the browser moves (I46, I48): the policy
        // already hard-denied a private literal, and the toolset refuses private addresses
        // and other schemes itself, so this is the length, budget and secret screen.
        let parsed = match self.shared.fetcher.check_url(&url) {
            Ok(parsed) => parsed,
            Err(refusal) => {
                if let Some(audit) = &audit {
                    let mut record = json!({
                        "functionCallId": call_id,
                        "tool": self.inner.name(),
                        "phase": "refused",
                        "urlDigest": url_digest,
                        "refusal": refusal.audit_refusal(),
                        "code": refusal.code(),
                    });
                    if refusal.url_is_loggable() {
                        record["host"] = json!(crate::web::url_host(&url));
                    }
                    audit.record(BROWSER_AUDIT_NAVIGATED, record).await?;
                }
                return Err(AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Forbidden,
                    refusal.code(),
                    refusal.to_string(),
                ));
            }
        };
        let allowlisted = host_allowlisted(&parsed.host, &self.shared.web.allowed_domains);
        // Write-ahead: the navigation is recorded before the browser moves (spec 3.1 §9.3).
        if let Some(audit) = &audit {
            audit
                .record(
                    BROWSER_AUDIT_NAVIGATED,
                    json!({
                        "functionCallId": call_id,
                        "tool": self.inner.name(),
                        "phase": "request",
                        "host": parsed.host,
                        "allowlisted": allowlisted,
                        "urlDigest": url_digest,
                    }),
                )
                .await?;
        }
        let result = self.inner.execute(ctx.clone(), args).await;
        if let Some(audit) = &audit {
            let final_host = result
                .as_ref()
                .ok()
                .and_then(|value| value.get("url"))
                .and_then(Value::as_str)
                .and_then(crate::web::url_host);
            audit
                .record(
                    BROWSER_AUDIT_NAVIGATED,
                    json!({
                        "functionCallId": call_id,
                        "tool": self.inner.name(),
                        "phase": "response",
                        "host": parsed.host,
                        "finalHost": final_host,
                        "allowlisted": allowlisted,
                        "urlDigest": url_digest,
                        "outcome": if result.is_ok() { "executed" } else { "failed" },
                    }),
                )
                .await?;
        }
        self.deliver(ctx.as_ref(), result?).await
    }

    async fn history(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let audit = HarnessAudit::for_tool(ctx.as_ref());
        let call_id = ctx.function_call_id().to_string();
        let result = self.inner.execute(ctx.clone(), args).await;
        if let Some(audit) = &audit {
            let final_host = result
                .as_ref()
                .ok()
                .and_then(|value| value.get("url"))
                .and_then(Value::as_str)
                .and_then(crate::web::url_host);
            audit
                .record(
                    BROWSER_AUDIT_NAVIGATED,
                    json!({
                        "functionCallId": call_id,
                        "tool": self.inner.name(),
                        "phase": "response",
                        "finalHost": final_host,
                        "allowlisted": false,
                        "outcome": if result.is_ok() { "executed" } else { "failed" },
                    }),
                )
                .await?;
        }
        self.deliver(ctx.as_ref(), result?).await
    }

    async fn act(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let audit = HarnessAudit::for_tool(ctx.as_ref());
        let call_id = ctx.function_call_id().to_string();
        // Text typed into a page that matches a secret pattern never leaves (I58); the
        // refusal is audited by digest, never by value.
        let typed: Vec<&str> = ["text", "key"]
            .iter()
            .filter_map(|field| args.get(*field).and_then(Value::as_str))
            .collect();
        if typed.iter().any(|text| has_secret(text)) {
            if let Some(audit) = &audit {
                audit
                    .record(
                        BROWSER_AUDIT_ACTED,
                        json!({
                            "functionCallId": call_id,
                            "tool": self.inner.name(),
                            "outcome": "refused",
                            "refusal": "secretInInput",
                            "inputDigest": hex_digest(typed.join("\u{1f}").as_bytes()),
                        }),
                    )
                    .await?;
            }
            return Err(refused(
                BROWSER_SECRET_IN_INPUT_CODE,
                "the text to type matches a secret pattern; the harness never types a \
                 credential into a page",
            ));
        }
        let result = self.inner.execute(ctx.clone(), args).await;
        let delivered = match result {
            Ok(value) => self.deliver(ctx.as_ref(), value).await,
            Err(error) => Err(error),
        };
        if let Some(audit) = &audit {
            audit
                .record(
                    BROWSER_AUDIT_ACTED,
                    json!({
                        "functionCallId": call_id,
                        "tool": self.inner.name(),
                        "outcome": if delivered.is_ok() { "executed" } else { "failed" },
                        "screened": delivered
                            .as_ref()
                            .ok()
                            .and_then(|value| value.get("screened"))
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    }),
                )
                .await?;
        }
        delivered
    }
}

#[async_trait]
impl Tool for GovernedBrowserTool {
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
        self.role == Role::Read
    }

    fn effect(&self) -> ToolEffect {
        match self.role {
            Role::Read => ToolEffect::ReadOnly,
            Role::Navigation | Role::History => ToolEffect::Idempotent,
            Role::Action => ToolEffect::NonIdempotent,
        }
    }

    fn timeout_override(&self) -> Option<Option<std::time::Duration>> {
        self.inner.timeout_override()
    }

    fn is_concurrency_safe(&self) -> bool {
        false
    }

    fn is_agent_delegation(&self) -> bool {
        false
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        match self.role {
            Role::Read => {
                let result = self.inner.execute(ctx.clone(), args).await?;
                self.deliver(ctx.as_ref(), result).await
            }
            Role::Navigation => self.navigate(ctx, args).await,
            Role::History => self.history(ctx, args).await,
            Role::Action => self.act(ctx, args).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_tool_has_one_class_and_the_two_refused_tools_have_none() {
        let mut seen = std::collections::HashSet::new();
        for (name, _) in browser_classes() {
            assert!(seen.insert(name), "{name} is listed twice");
        }
        assert_eq!(seen.len(), 44, "every tool of BrowserProfile::Full is classed");
        for name in BROWSER_NEVER_ATTACHED {
            assert_eq!(browser_class(name), None);
            assert!(is_browser_tool(name));
        }
        assert!(BROWSER_READ_TOOLS.iter().all(|name| !browser_member_excluded(name)));
        assert!(BROWSER_ACTION_TOOLS.iter().all(|name| browser_member_excluded(name)));
    }

    #[test]
    fn the_profile_tools_are_the_classed_tools_and_never_the_refused_ones() {
        let session = Arc::new(BrowserSession::new(BrowserConfig::default()));
        let names: Vec<String> = BrowserToolset::with_profile(session, BrowserProfile::Full)
            .all_tools()
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        for name in &names {
            assert!(browser_class(name).is_some(), "{name} is unclassed");
        }
        for name in BROWSER_NEVER_ATTACHED {
            assert!(!names.contains(&(*name).to_string()));
        }
    }

    #[test]
    fn roles_follow_the_class_lists() {
        assert_eq!(Role::of("browser_extract_text"), Some(Role::Read));
        assert_eq!(Role::of("browser_navigate"), Some(Role::Navigation));
        assert_eq!(Role::of("browser_back"), Some(Role::History));
        assert_eq!(Role::of("browser_type"), Some(Role::Action));
        assert_eq!(Role::of("browser_evaluate_js"), None);
    }
}
