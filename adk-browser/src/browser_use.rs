//! Claude's browser use toolset (`browser_toolset_20260801`) as governed ADK tools over
//! WebDriver.
//!
//! Each member is one ADK tool, `browser.<member>`, so tool policy, confirmations, standing
//! approvals, the kill switch, run budgets and the action ledger decide every action a model
//! asks for before the browser sees it. The members share one ordered batch: after a member
//! call fails or is refused or held, the rest of the model's turn is answered with the halt
//! text instead of running.

use crate::session::BrowserSession;
use crate::tools::UrlPolicy;
use crate::viewport::{self, Viewport};
use adk_core::egress::ToolEgress;
use adk_core::{
    AdkError, BatchGroup, InlineDataPart, MouseButton, ReadonlyContext, Result, ScreenPoint,
    ScrollDirection, Tool, ToolContext, ToolEffect, Toolset,
};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// The `toolset_name` and batch group of the browser use members.
pub const BROWSER_TOOLSET: &str = "browser";

/// The exact result text for each browser use call after the first failure in a batch.
pub const BROWSER_BATCH_HALT_TEXT: &str = "Not executed: an earlier action in this turn failed.";

const MEMBER_KEY: &str = "x-adk-anthropic-toolset-member";
const MAX_WAIT: Duration = Duration::from_secs(30);
const PAGE_LIMIT: usize = 50_000;
const FIELD_LIMIT: usize = 4_096;

macro_rules! browser_members {
    ($($variant:ident => $wire:literal, $doc:literal;)+) => {
        /// A member of Claude's browser use toolset.
        ///
        /// # Example
        ///
        /// ```rust
        /// use adk_browser::BrowserUseMember;
        ///
        /// assert_eq!(BrowserUseMember::ALL.len(), 31);
        /// assert_eq!(BrowserUseMember::ReadPage.tool_name(), "browser.read_page");
        /// ```
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum BrowserUseMember {
            $(#[doc = $doc] $variant,)+
        }

        impl BrowserUseMember {
            /// Every member.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The member's wire name.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                }
            }

            fn description(self) -> &'static str {
                match self {
                    $(Self::$variant => $doc,)+
                }
            }
        }
    };
}

browser_members! {
    Navigate => "navigate", "Load an http or https URL, or go back, forward or reload.";
    Screenshot => "screenshot", "Capture the viewport as an image.";
    Zoom => "zoom", "Capture a region of the viewport, upscaled, as an image.";
    LeftClick => "left_click", "Left-click a coordinate or a referenced element.";
    RightClick => "right_click", "Right-click a coordinate or a referenced element.";
    MiddleClick => "middle_click", "Middle-click a coordinate or a referenced element.";
    DoubleClick => "double_click", "Double-click a coordinate or a referenced element.";
    TripleClick => "triple_click", "Triple-click a coordinate or a referenced element.";
    Hover => "hover", "Move the pointer over a coordinate or a referenced element.";
    LeftClickDrag => "left_click_drag", "Press at one coordinate, drag to another, release.";
    LeftMouseDown => "left_mouse_down", "Press the left button at a coordinate.";
    LeftMouseUp => "left_mouse_up", "Release the left button at a coordinate.";
    MouseMove => "mouse_move", "Move the pointer to a coordinate.";
    Scroll => "scroll", "Scroll at a viewport position.";
    ScrollTo => "scroll_to", "Scroll a referenced element into view.";
    Type => "type", "Type a literal string at the focus.";
    Key => "key", "Press a key, chord or sequence.";
    HoldKey => "hold_key", "Hold a key or chord down for a duration.";
    Wait => "wait", "Pause for a duration.";
    ReadPage => "read_page", "Return the accessibility tree with element references.";
    Find => "find", "Find elements matching a description.";
    GetPageText => "get_page_text", "Return the page's visible text.";
    FormInput => "form_input", "Set a form element's value.";
    FileUpload => "file_upload", "Set the files on a file input from allowed directories.";
    ReadConsole => "read_console", "Return console entries since the last read.";
    ReadNetwork => "read_network", "Return network requests since the last read.";
    JavascriptExec => "javascript_exec", "Run JavaScript in the page.";
    NewTab => "new_tab", "Open a tab and make it active.";
    ListTabs => "list_tabs", "Report the tab inventory.";
    SwitchTab => "switch_tab", "Make a tab active.";
    CloseTab => "close_tab", "Close a tab.";
}

impl BrowserUseMember {
    /// The ADK tool name, `browser.<member>`.
    pub fn tool_name(self) -> String {
        format!("{BROWSER_TOOLSET}.{}", self.as_str())
    }

    /// Whether the member only reads the page or the tab inventory.
    pub fn is_observation(self) -> bool {
        matches!(
            self,
            Self::Screenshot
                | Self::Zoom
                | Self::ReadPage
                | Self::Find
                | Self::GetPageText
                | Self::ReadConsole
                | Self::ReadNetwork
                | Self::ListTabs
        )
    }

    fn is_tab_management(self) -> bool {
        matches!(self, Self::NewTab | Self::ListTabs | Self::SwitchTab | Self::CloseTab)
    }
}

/// Claude's browser use toolset over a WebDriver browser session.
///
/// It offers the 27 default members of `browser_toolset_20260801`; `javascript_exec` and
/// `file_upload` are offered only when enabled, and `read_console` and `read_network`, which
/// WebDriver cannot capture, are never offered. Navigation keeps the [`BrowserToolset`]
/// URL policy: `http` and `https` only, and no private network addresses, also after
/// redirects.
///
/// [`BrowserToolset`]: crate::BrowserToolset
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_browser::{BrowserConfig, BrowserSession, BrowserUseToolset};
///
/// let browser = Arc::new(BrowserSession::new(BrowserConfig::new().viewport(1280, 800)));
/// let toolset = BrowserUseToolset::new(browser);
/// assert_eq!(toolset.members().len(), 27);
/// ```
pub struct BrowserUseToolset {
    state: Arc<BrowserUse>,
    javascript: bool,
}

impl BrowserUseToolset {
    /// The toolset over `browser`.
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self {
            state: Arc::new(BrowserUse {
                viewport: Viewport::new(browser),
                policy: UrlPolicy::default(),
                upload_roots: None,
                egress: ToolEgress::Undeclared,
                tabs: Mutex::new(Tabs::default()),
            }),
            javascript: false,
        }
    }

    fn state_mut(&mut self) -> &mut BrowserUse {
        Arc::get_mut(&mut self.state).expect("configured before the tools are handed out")
    }

    /// Offers `javascript_exec`, which runs model-written JavaScript with the page's
    /// privileges. Enable it only in sessions that hold no credentials.
    pub fn with_javascript(mut self, enabled: bool) -> Self {
        self.javascript = enabled;
        self
    }

    /// Offers `file_upload`, reading only files under `roots`.
    pub fn with_file_upload<I, P>(mut self, roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.state_mut().upload_roots = Some(roots.into_iter().map(Into::into).collect());
        self
    }

    /// Lets navigation reach loopback, private and link-local addresses.
    pub fn with_private_network_access(mut self, enabled: bool) -> Self {
        self.state_mut().policy.allow_private_network = enabled;
        self
    }

    /// Lets navigation open host names that do not resolve on the agent host.
    pub fn with_unresolved_hosts(mut self, enabled: bool) -> Self {
        self.state_mut().policy.allow_unresolved_hosts = enabled;
        self
    }

    /// What every acting member other than `navigate` declares about its network use: a page
    /// can load anything after a click. Undeclared by default, which a run egress policy
    /// refuses; declare [`ToolEgress::Enforced`] when the browser's traffic passes through a
    /// proxy that enforces the run's policy.
    pub fn with_egress(mut self, egress: ToolEgress) -> Self {
        self.state_mut().egress = egress;
        self
    }

    /// The members the toolset offers.
    pub fn members(&self) -> Vec<BrowserUseMember> {
        BrowserUseMember::ALL
            .iter()
            .copied()
            .filter(|member| match member {
                BrowserUseMember::ReadConsole | BrowserUseMember::ReadNetwork => false,
                BrowserUseMember::JavascriptExec => self.javascript,
                BrowserUseMember::FileUpload => self.state.upload_roots.is_some(),
                _ => true,
            })
            .collect()
    }

    /// The member tools, one per offered member.
    pub fn member_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.members()
            .into_iter()
            .map(|member| {
                Arc::new(BrowserUseTool {
                    member,
                    name: member.tool_name(),
                    state: self.state.clone(),
                }) as Arc<dyn Tool>
            })
            .collect()
    }
}

#[async_trait]
impl Toolset for BrowserUseToolset {
    fn name(&self) -> &str {
        "browser_use"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        Ok(self.member_tools())
    }
}

/// The tab ids handed out for WebDriver window handles, with the last title and URL seen.
#[derive(Default)]
struct Tabs {
    ids: HashMap<String, String>,
    known: BTreeMap<u64, (String, String, String)>,
    next: u64,
}

impl Tabs {
    fn id_for(&mut self, handle: &str) -> String {
        if let Some(id) = self.ids.get(handle) {
            return id.clone();
        }
        self.next += 1;
        let id = format!("tab-{}", self.next);
        self.ids.insert(handle.to_string(), id.clone());
        self.known.insert(self.next, (id.clone(), String::new(), String::new()));
        id
    }

    fn handle_for(&self, tab_id: &str) -> Option<String> {
        self.ids.iter().find(|(_, id)| id.as_str() == tab_id).map(|(handle, _)| handle.clone())
    }

    fn remember(&mut self, tab_id: &str, title: &str, url: &str) {
        if let Some(entry) = self.known.values_mut().find(|(id, _, _)| id == tab_id) {
            entry.1 = title.to_string();
            entry.2 = url.to_string();
        }
    }

    fn info(&self, tab_id: &str) -> (String, String) {
        self.known
            .values()
            .find(|(id, _, _)| id == tab_id)
            .map(|(_, title, url)| (title.clone(), url.clone()))
            .unwrap_or_default()
    }
}

struct BrowserUse {
    viewport: Viewport,
    policy: UrlPolicy,
    upload_roots: Option<Vec<PathBuf>>,
    egress: ToolEgress,
    tabs: Mutex<Tabs>,
}

/// Text the API renders verbatim: no control characters, at most 4,096 characters.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() && *c != '\u{2028}' && *c != '\u{2029}')
        .take(FIELD_LIMIT)
        .collect()
}

fn webdriver(action: &str, error: impl std::fmt::Display) -> AdkError {
    AdkError::tool(format!("Error: {action} failed: {error}"))
}

/// A target: a viewport coordinate or an element reference.
enum Target {
    Point(ScreenPoint),
    Ref(String),
}

const READ_PAGE: &str = r#"
const [filter, maxDepth, rootRef, query, limit] = arguments;
let seq = window.__adkRefSeq || 0;
const TAG_ROLES = {A:'link',BUTTON:'button',SELECT:'combobox',TEXTAREA:'textbox',H1:'heading',H2:'heading',H3:'heading',H4:'heading',H5:'heading',H6:'heading',IMG:'img',NAV:'navigation',MAIN:'main',HEADER:'banner',FOOTER:'contentinfo',FORM:'form',UL:'list',OL:'list',LI:'listitem',TABLE:'table',TR:'row',TD:'cell',TH:'columnheader',LABEL:'label',P:'paragraph',ARTICLE:'article',DIALOG:'dialog',OPTION:'option',SUMMARY:'button'};
const INPUT_ROLES = {checkbox:'checkbox',radio:'radio',button:'button',submit:'button',reset:'button',image:'button',range:'slider',search:'searchbox',number:'spinbutton',file:'button'};
const clean = (s) => (s || '').replace(/\s+/g, ' ').trim();
function role(el) {
  const r = el.getAttribute('role'); if (r) return r;
  if (el.tagName === 'INPUT') { const t = (el.getAttribute('type') || 'text').toLowerCase(); return t === 'hidden' ? null : (INPUT_ROLES[t] || 'textbox'); }
  if (el.tagName === 'A' && !el.hasAttribute('href')) return null;
  return TAG_ROLES[el.tagName] || null;
}
function interactive(el) {
  if (['BUTTON','INPUT','SELECT','TEXTAREA','SUMMARY'].includes(el.tagName)) return el.type !== 'hidden';
  if (el.tagName === 'A') return el.hasAttribute('href');
  const r = el.getAttribute('role');
  if (r && /^(button|link|checkbox|radio|tab|menuitem|option|switch|textbox|combobox|searchbox|slider)$/.test(r)) return true;
  if (el.isContentEditable) return true;
  const ti = el.getAttribute('tabindex');
  return (ti !== null && Number(ti) >= 0) || typeof el.onclick === 'function';
}
function shown(el, all) {
  const s = getComputedStyle(el);
  if (s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
  const r = el.getBoundingClientRect();
  if (r.width === 0 && r.height === 0) return false;
  return all || (r.bottom > 0 && r.right > 0 && r.top < innerHeight && r.left < innerWidth);
}
function name(el) {
  let n = el.getAttribute('aria-label'); if (n) return clean(n);
  const by = el.getAttribute('aria-labelledby');
  if (by) { n = by.split(/\s+/).map((id) => { const e = document.getElementById(id); return e ? e.innerText : ''; }).join(' '); if (clean(n)) return clean(n); }
  if (el.labels && el.labels.length) return clean(Array.from(el.labels).map((l) => l.innerText).join(' '));
  n = el.getAttribute('alt') || el.getAttribute('title') || el.getAttribute('placeholder'); if (n) return clean(n);
  if (el.tagName === 'INPUT' && /^(submit|button|reset)$/i.test(el.type)) return clean(el.value);
  if (['INPUT','SELECT','TEXTAREA'].includes(el.tagName)) return '';
  return clean(el.innerText).slice(0, 100);
}
function ref(el) {
  let r = el.getAttribute('data-adk-ref');
  if (!r) { seq += 1; r = 'ref_' + seq; el.setAttribute('data-adk-ref', r); }
  return r;
}
function line(el, r) {
  const extra = [];
  if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') {
    if (el.type === 'checkbox' || el.type === 'radio') extra.push(el.checked ? 'checked' : 'unchecked');
    else if (el.value) extra.push('value="' + String(el.value).slice(0, 100).replace(/"/g, '\\"') + '"');
  }
  if (el.tagName === 'SELECT' && el.selectedIndex >= 0) extra.push('value="' + clean(el.options[el.selectedIndex].text) + '"');
  if (el.tagName === 'A' && el.href) extra.push('href="' + el.href + '"');
  if (/^H[1-6]$/.test(el.tagName)) extra.push('level=' + el.tagName[1]);
  if (el.disabled) extra.push('disabled');
  return r + ' "' + name(el).replace(/"/g, '\\"') + '"' + (extra.length ? ' ' + extra.join(' ') : '') + ' [' + ref(el) + ']';
}
const lines = []; let size = 0; let truncated = false;
function emit(text) { if (size + text.length + 1 > limit) { truncated = true; return; } lines.push(text); size += text.length + 1; }
let root = document.body;
if (rootRef) { root = document.querySelector('[data-adk-ref="' + CSS.escape(rootRef) + '"]'); if (!root) return { stale: true }; }
if (query) {
  const words = clean(query).toLowerCase().split(' ').filter((w) => w.length > 1);
  const scored = [];
  for (const el of document.body.querySelectorAll('*')) {
    const r = role(el); if (!r && !interactive(el)) continue;
    if (getComputedStyle(el).display === 'none' || !shown(el, true)) continue;
    const hay = ((r || '') + ' ' + name(el) + ' ' + (el.getAttribute('type') || '') + ' ' + (el.getAttribute('placeholder') || '')).toLowerCase();
    const score = words.filter((w) => hay.includes(w)).length;
    if (score > 0) scored.push([score + (interactive(el) ? 0.5 : 0), el, r || 'generic']);
  }
  scored.sort((a, b) => b[0] - a[0]);
  for (const [, el, r] of scored.slice(0, 20)) emit(line(el, r));
} else {
  const all = filter === 'all';
  const walk = (el, level) => {
    if (truncated || !(el instanceof Element)) return;
    if (getComputedStyle(el).display === 'none') return;
    const r = role(el); const inter = interactive(el);
    const include = filter === 'interactive' ? inter && shown(el, all) : (r !== null || inter) && shown(el, all);
    let next = level;
    if (include) { emit('  '.repeat(filter === 'interactive' ? 0 : level) + line(el, r || 'generic')); next = level + 1; }
    if (next >= maxDepth) return;
    for (const child of el.children) walk(child, next);
  };
  walk(root, 0);
}
window.__adkRefSeq = seq;
return { text: lines.join('\n'), truncated, count: lines.length };
"#;

const PAGE_TEXT: &str = r#"
const root = document.querySelector('main, article, [role=main]') || document.body;
return (root ? root.innerText : '').slice(0, arguments[0]);
"#;

const RESOLVE_REF: &str = r#"
const el = document.querySelector('[data-adk-ref="' + CSS.escape(arguments[0]) + '"]');
if (!el) return null;
el.scrollIntoView({ block: 'center', inline: 'center' });
const r = el.getBoundingClientRect();
return [r.left + r.width / 2, r.top + r.height / 2];
"#;

const FORM_INPUT: &str = r#"
const [ref, value] = arguments;
const el = document.querySelector('[data-adk-ref="' + CSS.escape(ref) + '"]');
if (!el) return { stale: true };
const fire = (type) => el.dispatchEvent(new Event(type, { bubbles: true }));
if (el.tagName === 'SELECT') {
  const want = String(value);
  const option = Array.from(el.options).find((o) => o.value === want || o.text.trim() === want);
  if (!option) return { error: 'no option "' + want + '" in the select' };
  el.value = option.value; fire('input'); fire('change');
  return { ok: 'Selected "' + option.text.trim() + '".' };
}
if (el.type === 'checkbox' || el.type === 'radio') {
  const want = value === true || value === 'true';
  if (el.checked !== want) el.click();
  return { ok: (el.checked ? 'Checked ' : 'Unchecked ') + ref + '.' };
}
if (!('value' in el) && !el.isContentEditable) return { error: ref + ' is not a form element' };
el.focus();
if (el.isContentEditable) { el.textContent = String(value); }
else {
  const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(proto, 'value');
  if (setter && setter.set) setter.set.call(el, String(value)); else el.value = String(value);
}
fire('input'); fire('change');
return { ok: 'Set ' + ref + ' to "' + String(value).slice(0, 100) + '".' };
"#;

const SCROLL_TO: &str = r#"
const el = document.querySelector('[data-adk-ref="' + CSS.escape(arguments[0]) + '"]');
if (!el) return false;
el.scrollIntoView({ block: 'center', inline: 'nearest' });
return true;
"#;

const JAVASCRIPT_EXEC: &str = r#"
const value = (0, eval)(arguments[0]);
if (value === undefined) return 'undefined';
if (typeof value === 'string') return value;
try { return JSON.stringify(value) ?? String(value); } catch (e) { return String(value); }
"#;

fn stale(reference: &str) -> AdkError {
    AdkError::tool(format!(
        "Error: {reference} is stale or not found on the current page. Re-read the page to get \
         fresh references."
    ))
}

impl BrowserUse {
    /// The inventory of open tabs, with the active one marked.
    async fn state(&self, opened: &[String]) -> Result<Value> {
        let driver = self.viewport.driver().await?;
        let handles = driver.windows().await.map_err(|error| webdriver("listing tabs", error))?;
        let active =
            driver.window().await.map_err(|error| webdriver("reading the active tab", error))?;
        let title = driver.title().await.unwrap_or_default();
        let url = driver.current_url().await.map(|url| url.to_string()).unwrap_or_default();
        let mut tabs = self.tabs.lock().await;
        let mut entries = Vec::new();
        for handle in &handles {
            let id = tabs.id_for(&handle.to_string());
            let is_active = handle.to_string() == active.to_string();
            if is_active {
                tabs.remember(&id, &title, &url);
            }
            let (title, url) = tabs.info(&id);
            let mut entry = json!({ "tab_id": id, "title": clean(&title), "url": clean(&url) });
            if is_active {
                entry["active"] = json!(true);
            }
            entries.push(entry);
        }
        let mut state = json!({ "tabs": entries });
        let changes: Vec<Value> = opened
            .iter()
            .map(|handle| json!({ "type": "tab_opened", "tab_id": tabs.id_for(handle) }))
            .collect();
        if !changes.is_empty() {
            state["state_changes"] = Value::Array(changes);
        }
        Ok(state)
    }

    async fn handles(&self) -> Result<Vec<String>> {
        let driver = self.viewport.driver().await?;
        let handles = driver.windows().await.map_err(|error| webdriver("listing tabs", error))?;
        Ok(handles.into_iter().map(|handle| handle.to_string()).collect())
    }

    /// Makes `tab_id` active when given and not already.
    async fn focus(&self, tab_id: Option<&str>) -> Result<()> {
        let Some(tab_id) = tab_id else { return Ok(()) };
        let handle = {
            let tabs = self.tabs.lock().await;
            tabs.handle_for(tab_id)
        };
        let Some(handle) = handle else {
            return Err(AdkError::tool(format!("Error: no open tab has tab_id {tab_id}.")));
        };
        let driver = self.viewport.driver().await?;
        let active =
            driver.window().await.map_err(|error| webdriver("reading the active tab", error))?;
        if active.to_string() != handle {
            driver
                .switch_to_window(thirtyfour::WindowHandle::from(handle))
                .await
                .map_err(|error| webdriver("switching tabs", error))?;
            self.viewport.reset_frame();
        }
        Ok(())
    }

    /// A target in screenshot pixels; a reference is scrolled into view first.
    async fn point(&self, target: &Target) -> Result<ScreenPoint> {
        match target {
            Target::Point(point) => Ok(*point),
            Target::Ref(reference) => {
                let center =
                    self.viewport.run("resolve_ref", RESOLVE_REF, vec![json!(reference)]).await?;
                let (Some(x), Some(y)) = (center[0].as_f64(), center[1].as_f64()) else {
                    return Err(stale(reference));
                };
                self.viewport.css_to_screen(x, y).await
            }
        }
    }

    async fn navigate(&self, url: &str) -> Result<String> {
        let driver = self.viewport.driver().await?;
        match url {
            "back" => driver.back().await.map_err(|error| webdriver("going back", error))?,
            "forward" => {
                driver.forward().await.map_err(|error| webdriver("going forward", error))?
            }
            "reload" => driver.refresh().await.map_err(|error| webdriver("reloading", error))?,
            _ => {
                let url = normalize_url(url);
                self.policy.check(&url).await.map_err(|error| {
                    AdkError::tool(format!("Error: navigation refused. {}", error.message))
                })?;
                driver.goto(url.as_str()).await.map_err(|error| webdriver("navigation", error))?;
            }
        }
        self.viewport.reset_frame();
        let landed = self.policy.check_landing(self.viewport.session()).await?;
        Ok(format!("Navigated to {}", clean(&landed)))
    }
}

/// A URL without a scheme is an `https` URL; `host:port` is a host, not a scheme.
fn normalize_url(url: &str) -> String {
    let url = url.trim();
    let has_scheme = url.split_once(':').is_some_and(|(scheme, rest)| {
        !scheme.is_empty()
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-'))
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && !rest.starts_with(|c: char| c.is_ascii_digit())
    });
    if has_scheme { url.to_string() } else { format!("https://{url}") }
}

struct BrowserUseTool {
    member: BrowserUseMember,
    name: String,
    state: Arc<BrowserUse>,
}

fn invalid(member: BrowserUseMember, message: impl std::fmt::Display) -> AdkError {
    AdkError::tool(format!("Error: invalid input for {}: {message}", member.as_str()))
}

impl BrowserUseTool {
    fn target(&self, input: &Map<String, Value>, field: &str) -> Result<Target> {
        let target = input.get(field).and_then(Value::as_object).ok_or_else(|| {
            invalid(self.member, format!("{field} must be a coordinate or ref target"))
        })?;
        match target.get("type").and_then(Value::as_str) {
            Some("coordinate") => match (
                target.get("x").and_then(Value::as_i64),
                target.get("y").and_then(Value::as_i64),
            ) {
                (Some(x), Some(y)) => Ok(Target::Point(ScreenPoint::new(x, y))),
                _ => Err(invalid(self.member, format!("{field} needs integer x and y"))),
            },
            Some("ref") => target
                .get("ref")
                .and_then(Value::as_str)
                .map(|reference| Target::Ref(reference.to_string()))
                .ok_or_else(|| invalid(self.member, format!("{field} needs a ref"))),
            _ => Err(invalid(self.member, format!("{field}.type must be coordinate or ref"))),
        }
    }

    fn coordinate(&self, input: &Map<String, Value>, field: &str) -> Result<ScreenPoint> {
        match self.target(input, field)? {
            Target::Point(point) => Ok(point),
            Target::Ref(_) => {
                Err(invalid(self.member, format!("{field} must be a coordinate target")))
            }
        }
    }

    fn reference(&self, input: &Map<String, Value>, field: &str) -> Result<String> {
        match self.target(input, field)? {
            Target::Ref(reference) => Ok(reference),
            Target::Point(_) => Err(invalid(self.member, format!("{field} must be a ref target"))),
        }
    }

    fn text<'a>(&self, input: &'a Map<String, Value>) -> Result<&'a str> {
        input
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(self.member, "text is required"))
    }

    fn duration(&self, input: &Map<String, Value>) -> Result<Duration> {
        let seconds = input
            .get("duration")
            .and_then(Value::as_f64)
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .ok_or_else(|| invalid(self.member, "duration must be 0 to 30 seconds"))?;
        Ok(Duration::from_secs_f64(seconds).min(MAX_WAIT))
    }

    fn modifiers(&self, input: &Map<String, Value>) -> Result<Vec<char>> {
        match input.get("modifiers").and_then(Value::as_str) {
            Some(chord) => viewport::chord(chord),
            None => Ok(Vec::new()),
        }
    }

    /// Runs an acting member and reports its text with the tab state, naming any tab the
    /// action opened.
    async fn act<F, Fut>(&self, action: F) -> Result<Value>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<String>>,
    {
        let before = self.state.handles().await?;
        let text = action().await?;
        let after = self.state.handles().await?;
        let opened: Vec<String> =
            after.into_iter().filter(|handle| !before.contains(handle)).collect();
        Ok(json!({ "text": text, "browser_state": self.state.state(&opened).await? }))
    }

    async fn run(&self, input: Map<String, Value>) -> Result<Value> {
        let input = &input;
        let state = &self.state;
        let viewport = &state.viewport;
        let tab_id = input.get("tab_id").and_then(Value::as_str);
        if !matches!(self.member, BrowserUseMember::SwitchTab | BrowserUseMember::CloseTab) {
            state.focus(tab_id).await?;
        }
        let click = |button: MouseButton, count: u8, verb: &'static str| async move {
            let target = self.target(input, "target")?;
            let modifiers = self.modifiers(input)?;
            let point = state.point(&target).await?;
            viewport.click(Some(point), button, count, &modifiers).await?;
            Ok(match target {
                Target::Ref(reference) => format!("{verb} element {reference}."),
                Target::Point(point) => format!("{verb} at ({}, {}).", point.x, point.y),
            })
        };
        Ok(match self.member {
            BrowserUseMember::Navigate => {
                let url = input
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid(self.member, "url is required"))?;
                self.act(|| state.navigate(url)).await?
            }
            BrowserUseMember::Screenshot => {
                let shot = viewport.screenshot().await?;
                image_result("Screenshot captured.", shot)
            }
            BrowserUseMember::Zoom => {
                let region: Vec<i64> = input
                    .get("region")
                    .and_then(Value::as_array)
                    .map(|region| region.iter().filter_map(Value::as_i64).collect())
                    .unwrap_or_default();
                let [x0, y0, x1, y1] = region[..] else {
                    return Err(invalid(self.member, "region must be [x0, y0, x1, y1]"));
                };
                image_result("Zoomed region captured.", viewport.zoom([x0, y0, x1, y1]).await?)
            }
            BrowserUseMember::LeftClick => {
                self.act(|| click(MouseButton::Left, 1, "Clicked")).await?
            }
            BrowserUseMember::RightClick => {
                self.act(|| click(MouseButton::Right, 1, "Right-clicked")).await?
            }
            BrowserUseMember::MiddleClick => {
                self.act(|| click(MouseButton::Middle, 1, "Middle-clicked")).await?
            }
            BrowserUseMember::DoubleClick => {
                self.act(|| click(MouseButton::Left, 2, "Double-clicked")).await?
            }
            BrowserUseMember::TripleClick => {
                self.act(|| click(MouseButton::Left, 3, "Triple-clicked")).await?
            }
            BrowserUseMember::Hover => {
                let target = self.target(input, "target")?;
                let point = state.point(&target).await?;
                viewport.move_to(point).await?;
                Value::String("Hovered.".into())
            }
            BrowserUseMember::LeftClickDrag => {
                let from = self.coordinate(input, "from")?;
                let to = self.coordinate(input, "target")?;
                let modifiers = self.modifiers(input)?;
                self.act(|| async move {
                    viewport.drag(from, to, &modifiers).await?;
                    Ok(format!("Dragged from ({}, {}) to ({}, {}).", from.x, from.y, to.x, to.y))
                })
                .await?
            }
            BrowserUseMember::LeftMouseDown => {
                viewport.button(Some(self.coordinate(input, "target")?), true).await?;
                Value::String("Pressed the left button.".into())
            }
            BrowserUseMember::LeftMouseUp => {
                let at = self.coordinate(input, "target")?;
                self.act(|| async move {
                    viewport.button(Some(at), false).await?;
                    Ok("Released the left button.".to_string())
                })
                .await?
            }
            BrowserUseMember::MouseMove => {
                viewport.move_to(self.coordinate(input, "target")?).await?;
                Value::String("Moved the pointer.".into())
            }
            BrowserUseMember::Scroll => {
                let at = self.coordinate(input, "target")?;
                let direction = match input.get("scroll_direction").and_then(Value::as_str) {
                    Some("up") => ScrollDirection::Up,
                    Some("down") => ScrollDirection::Down,
                    Some("left") => ScrollDirection::Left,
                    Some("right") => ScrollDirection::Right,
                    _ => {
                        return Err(invalid(
                            self.member,
                            "scroll_direction must be up, down, left or right",
                        ));
                    }
                };
                let amount =
                    input.get("scroll_amount").and_then(Value::as_u64).unwrap_or(3).clamp(1, 10)
                        as u32;
                viewport.scroll(Some(at), direction, amount).await?;
                Value::String(format!(
                    "Scrolled {} by {amount}.",
                    input["scroll_direction"].as_str().unwrap_or_default()
                ))
            }
            BrowserUseMember::ScrollTo => {
                let reference = self.reference(input, "target")?;
                let found = viewport.run("scroll_to", SCROLL_TO, vec![json!(reference)]).await?;
                if found != Value::Bool(true) {
                    return Err(stale(&reference));
                }
                Value::String(format!("Scrolled {reference} into view."))
            }
            BrowserUseMember::Type => {
                let text = self.text(input)?.to_string();
                self.act(|| async move {
                    viewport.type_text(&text).await?;
                    Ok(format!("Typed {} characters.", text.chars().count()))
                })
                .await?
            }
            BrowserUseMember::Key => {
                let keys = self.text(input)?.to_string();
                let repeat =
                    input.get("repeat").and_then(Value::as_u64).unwrap_or(1).clamp(1, 100) as u32;
                self.act(|| async move {
                    viewport.press(&keys, repeat).await?;
                    Ok(format!("Pressed {keys}."))
                })
                .await?
            }
            BrowserUseMember::HoldKey => {
                let keys = self.text(input)?;
                viewport.hold(keys, self.duration(input)?).await?;
                Value::String(format!("Held {keys}."))
            }
            BrowserUseMember::Wait => {
                let duration = self.duration(input)?;
                tokio::time::sleep(duration).await;
                Value::String(format!("Waited {:.1} seconds.", duration.as_secs_f64()))
            }
            BrowserUseMember::ReadPage | BrowserUseMember::Find => {
                let (filter, depth, root, query) = if self.member == BrowserUseMember::Find {
                    let query = input
                        .get("query")
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid(self.member, "query is required"))?;
                    (Value::Null, json!(15), Value::Null, json!(query))
                } else {
                    let depth = input.get("depth").and_then(Value::as_u64).unwrap_or(15).max(1);
                    (
                        input.get("filter").cloned().unwrap_or(Value::Null),
                        json!(depth),
                        input.get("ref").cloned().unwrap_or(Value::Null),
                        Value::Null,
                    )
                };
                let page = viewport
                    .run(
                        "read_page",
                        READ_PAGE,
                        vec![filter, depth, root.clone(), query, json!(PAGE_LIMIT)],
                    )
                    .await?;
                if page["stale"] == Value::Bool(true) {
                    return Err(stale(root.as_str().unwrap_or_default()));
                }
                let mut text = page["text"].as_str().unwrap_or_default().to_string();
                if text.is_empty() {
                    text = if self.member == BrowserUseMember::Find {
                        "No matching elements.".into()
                    } else {
                        "The page has no visible elements.".into()
                    };
                }
                if page["truncated"] == Value::Bool(true) {
                    text.push_str("\n[Output truncated at 50,000 characters; read a smaller depth or a ref's subtree.]");
                }
                Value::String(text)
            }
            BrowserUseMember::GetPageText => {
                let text =
                    viewport.run("get_page_text", PAGE_TEXT, vec![json!(PAGE_LIMIT)]).await?;
                Value::String(text.as_str().unwrap_or_default().to_string())
            }
            BrowserUseMember::FormInput => {
                let reference = self.reference(input, "target")?;
                let value = input
                    .get("value")
                    .cloned()
                    .ok_or_else(|| invalid(self.member, "value is required"))?;
                let outcome =
                    viewport.run("form_input", FORM_INPUT, vec![json!(reference), value]).await?;
                if outcome["stale"] == Value::Bool(true) {
                    return Err(stale(&reference));
                }
                if let Some(error) = outcome["error"].as_str() {
                    return Err(AdkError::tool(format!("Error: {error}")));
                }
                Value::String(outcome["ok"].as_str().unwrap_or("Set.").to_string())
            }
            BrowserUseMember::FileUpload => {
                let reference = self.reference(input, "target")?;
                if input
                    .get("document_ids")
                    .is_some_and(|ids| ids.as_array().is_some_and(|ids| !ids.is_empty()))
                {
                    return Err(AdkError::tool(
                        "Error: document_ids are not staged in this environment; pass paths",
                    ));
                }
                let paths: Vec<&str> = input
                    .get("paths")
                    .and_then(Value::as_array)
                    .map(|paths| paths.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if paths.is_empty() {
                    return Err(invalid(self.member, "paths is required"));
                }
                let roots = state.upload_roots.as_deref().unwrap_or_default();
                let mut resolved = Vec::new();
                for path in paths {
                    resolved.push(crate::tools::resolve_upload(roots, path)?);
                }
                let driver = viewport.driver().await?;
                let element = driver
                    .find(thirtyfour::By::Css(format!("[data-adk-ref=\"{reference}\"]")))
                    .await
                    .map_err(|_| stale(&reference))?;
                let joined = resolved.join("\n");
                element.send_keys(joined).await.map_err(|error| webdriver("file upload", error))?;
                Value::String(format!("Set {} file(s) on {reference}.", resolved.len()))
            }
            BrowserUseMember::JavascriptExec => {
                let code = self.text(input)?.to_string();
                let value =
                    viewport.run("javascript_exec", JAVASCRIPT_EXEC, vec![json!(code)]).await?;
                let text = match value {
                    Value::String(text) => text,
                    other => other.to_string(),
                };
                Value::String(text.chars().take(PAGE_LIMIT).collect())
            }
            BrowserUseMember::ReadConsole | BrowserUseMember::ReadNetwork => {
                return Err(AdkError::tool(format!(
                    "Error: {} is not enabled in this environment.",
                    self.member.as_str()
                )));
            }
            BrowserUseMember::NewTab => {
                let driver = viewport.driver().await?;
                let before = state.handles().await?;
                let handle =
                    driver.new_tab().await.map_err(|error| webdriver("opening a tab", error))?;
                driver
                    .switch_to_window(handle.clone())
                    .await
                    .map_err(|error| webdriver("switching tabs", error))?;
                viewport.reset_frame();
                let opened: Vec<String> =
                    state.handles().await?.into_iter().filter(|h| !before.contains(h)).collect();
                json!({ "browser_state": state.state(&opened).await? })
            }
            BrowserUseMember::ListTabs => json!({ "browser_state": state.state(&[]).await? }),
            BrowserUseMember::SwitchTab => {
                let tab_id = tab_id.ok_or_else(|| invalid(self.member, "tab_id is required"))?;
                state.focus(Some(tab_id)).await?;
                json!({ "browser_state": state.state(&[]).await? })
            }
            BrowserUseMember::CloseTab => {
                let tab_id = tab_id.ok_or_else(|| invalid(self.member, "tab_id is required"))?;
                state.focus(Some(tab_id)).await?;
                let driver = viewport.driver().await?;
                driver.close_window().await.map_err(|error| webdriver("closing the tab", error))?;
                if let Some(next) = state.handles().await?.first() {
                    driver
                        .switch_to_window(thirtyfour::WindowHandle::from(next.clone()))
                        .await
                        .map_err(|error| webdriver("switching tabs", error))?;
                }
                viewport.reset_frame();
                json!({ "browser_state": state.state(&[]).await? })
            }
        })
    }
}

fn image_result(caption: &str, shot: adk_core::Screenshot) -> Value {
    let inline =
        InlineDataPart { mime_type: shot.mime_type, data: shot.data, uri: None, annotations: None };
    json!({ "response": { "text": caption }, "inline_data": [inline] })
}

#[async_trait]
impl Tool for BrowserUseTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        self.member.description()
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description(),
            MEMBER_KEY: { "toolset": BROWSER_TOOLSET, "member": self.member.as_str() }
        })
    }

    fn is_read_only(&self) -> bool {
        self.member.is_observation()
    }

    fn effect(&self) -> ToolEffect {
        if self.is_read_only() { ToolEffect::ReadOnly } else { ToolEffect::NonIdempotent }
    }

    fn batch_group(&self) -> Option<BatchGroup> {
        Some(BatchGroup::new(BROWSER_TOOLSET, BROWSER_BATCH_HALT_TEXT))
    }

    fn egress(&self, args: &Value) -> ToolEgress {
        match self.member {
            BrowserUseMember::Navigate => match args.get("url").and_then(Value::as_str) {
                Some("back" | "forward" | "reload") | None => self.state.egress.clone(),
                Some(url) => ToolEgress::for_urls([normalize_url(url)]),
            },
            member if member.is_observation() || member.is_tab_management() => ToolEgress::None,
            BrowserUseMember::Wait | BrowserUseMember::ScrollTo => ToolEgress::None,
            _ => self.state.egress.clone(),
        }
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let input = match args {
            Value::Object(input) => input,
            Value::Null => Map::new(),
            _ => return Err(invalid(self.member, "input must be an object")),
        };
        tracing::debug!(browser.member = self.member.as_str(), "performing browser action");
        self.run(input).await
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_url;

    #[test]
    fn urls_without_a_scheme_are_https() {
        assert_eq!(normalize_url("example.com/docs"), "https://example.com/docs");
        assert_eq!(normalize_url("localhost:8080"), "https://localhost:8080");
        assert_eq!(normalize_url("http://example.com"), "http://example.com");
        assert_eq!(normalize_url("javascript:alert(1)"), "javascript:alert(1)");
        assert_eq!(normalize_url("file:///etc/passwd"), "file:///etc/passwd");
        assert_eq!(normalize_url("about:blank"), "about:blank");
    }
}
