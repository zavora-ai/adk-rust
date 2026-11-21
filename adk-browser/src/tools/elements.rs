//! `browser_elements`: the page's interactive elements with their rectangles and a stable
//! selector each, for set-of-marks grounding (spec 3.7 §7.14, X16).

use crate::session::BrowserSession;
use adk_core::{Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Most elements one call returns unless `limit` says otherwise.
const DEFAULT_LIMIT: u64 = 200;
/// Most elements one call may return.
const MAX_LIMIT: u64 = 500;

/// Collects the visible interactive elements in the viewport. Each selector is checked to
/// match exactly one element, falling back to a structural path from the nearest unique id.
const ELEMENTS_SCRIPT: &str = r#"
const LIMIT = __LIMIT__;
const QUERY = 'a[href], button, input:not([type=hidden]), select, textarea, summary, label[for], '
  + '[role=button], [role=link], [role=checkbox], [role=radio], [role=tab], [role=menuitem], '
  + '[role=option], [role=switch], [role=textbox], [role=combobox], [role=searchbox], '
  + '[contenteditable=""], [contenteditable=true], [onclick], [tabindex]:not([tabindex="-1"])';
const unique = (sel) => { try { return document.querySelectorAll(sel).length === 1; } catch (e) { return false; } };
const quote = (v) => '"' + v.replace(/\\/g, '\\\\').replace(/"/g, '\\"') + '"';
function selectorFor(el) {
  if (el.id && unique('#' + CSS.escape(el.id))) return '#' + CSS.escape(el.id);
  const tag = el.tagName.toLowerCase();
  for (const attr of ['data-testid', 'data-test', 'data-qa', 'name', 'aria-label']) {
    const v = el.getAttribute(attr);
    if (v) { const s = tag + '[' + attr + '=' + quote(v) + ']'; if (unique(s)) return s; }
  }
  const parts = [];
  let node = el;
  while (node && node.nodeType === 1 && node !== document.documentElement) {
    if (node !== el && node.id && unique('#' + CSS.escape(node.id))) { parts.unshift('#' + CSS.escape(node.id)); break; }
    let i = 1, sib = node;
    while ((sib = sib.previousElementSibling)) if (sib.tagName === node.tagName) i++;
    parts.unshift(node.tagName.toLowerCase() + ':nth-of-type(' + i + ')');
    node = node.parentElement;
  }
  if (!parts.length || !parts[0].startsWith('#')) parts.unshift('html');
  return parts.join(' > ');
}
function roleOf(el) {
  const role = el.getAttribute('role');
  if (role) return role;
  const tag = el.tagName.toLowerCase();
  if (tag === 'a') return 'link';
  if (tag === 'select') return 'combobox';
  if (tag === 'textarea') return 'textbox';
  if (tag === 'input') {
    const type = (el.getAttribute('type') || 'text').toLowerCase();
    if (['button', 'submit', 'reset', 'image'].includes(type)) return 'button';
    if (['checkbox', 'radio', 'range'].includes(type)) return type === 'range' ? 'slider' : type;
    return type === 'search' ? 'searchbox' : 'textbox';
  }
  return tag;
}
function nameOf(el) {
  const clip = (t) => (t || '').replace(/\s+/g, ' ').trim().slice(0, 80);
  const label = el.getAttribute('aria-label');
  if (label) return clip(label);
  const by = el.getAttribute('aria-labelledby');
  if (by) { const t = by.split(/\s+/).map((id) => document.getElementById(id)).filter(Boolean).map((n) => n.textContent).join(' '); if (clip(t)) return clip(t); }
  if (el.labels && el.labels.length) return clip(Array.from(el.labels).map((l) => l.textContent).join(' '));
  const tag = el.tagName.toLowerCase();
  if (tag === 'input' && ['button', 'submit', 'reset'].includes((el.type || '').toLowerCase())) return clip(el.value);
  const text = clip(el.innerText || el.textContent);
  if (text) return text;
  return clip(el.getAttribute('alt') || el.getAttribute('title') || el.getAttribute('placeholder') || '');
}
const vw = window.innerWidth, vh = window.innerHeight;
const round = (v) => Math.round(v * 100) / 100;
const seen = new Set();
const elements = [];
let truncated = false;
for (const el of document.querySelectorAll(QUERY)) {
  if (seen.has(el)) continue;
  seen.add(el);
  if (el.disabled) continue;
  const r = el.getBoundingClientRect();
  if (r.width < 1 || r.height < 1 || r.right <= 0 || r.bottom <= 0 || r.left >= vw || r.top >= vh) continue;
  const style = getComputedStyle(el);
  if (style.visibility === 'hidden' || style.display === 'none' || Number(style.opacity) === 0) continue;
  if (elements.length >= LIMIT) { truncated = true; break; }
  elements.push({
    index: elements.length + 1,
    selector: selectorFor(el),
    role: roleOf(el),
    name: nameOf(el),
    tag: el.tagName.toLowerCase(),
    rect: { x: round(r.left), y: round(r.top), width: round(r.width), height: round(r.height) },
  });
}
return {
  url: location.href,
  devicePixelRatio: window.devicePixelRatio || 1,
  viewport: { width: vw, height: vh },
  elements,
  truncated,
};
"#;

/// Lists the visible interactive elements in the viewport: each with its rectangle in CSS
/// pixels, its role and accessible name, and a CSS selector that matches only it.
///
/// Draw them as numbered marks on a `browser_screenshot` with `vision_annotate`
/// (`units: "css"` and the answer's `devicePixelRatio`), ask the model for a number, and
/// act on the chosen element with `browser_click` and its selector — no coordinate click.
///
/// # Example
///
/// ```rust,ignore
/// use adk_browser::{BrowserConfig, BrowserSession, ElementsTool};
/// use std::sync::Arc;
///
/// let browser = Arc::new(BrowserSession::new(BrowserConfig::default()));
/// let tool = ElementsTool::new(browser);
/// ```
pub struct ElementsTool {
    browser: Arc<BrowserSession>,
}

impl ElementsTool {
    /// The tool over a shared browser session.
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for ElementsTool {
    fn name(&self) -> &str {
        "browser_elements"
    }

    fn description(&self) -> &str {
        "List the visible interactive elements in the viewport: links, buttons, inputs and \
         controls, each with its rectangle in CSS pixels, its role and name, and a CSS selector \
         that matches only it. Draw them as numbered marks on a screenshot with vision_annotate \
         (units css, this answer's devicePixelRatio) and click the chosen element with \
         browser_click and its selector."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LIMIT,
                    "description": format!("Most elements to return, in document order (default {DEFAULT_LIMIT}).")
                }
            }
        }))
    }

    fn response_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "url": { "type": "string" },
                "devicePixelRatio": { "type": "number" },
                "viewport": {
                    "type": "object",
                    "properties": { "width": { "type": "number" }, "height": { "type": "number" } }
                },
                "elements": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "index": { "type": "integer" },
                            "selector": { "type": "string" },
                            "role": { "type": "string" },
                            "name": { "type": "string" },
                            "tag": { "type": "string" },
                            "rect": {
                                "type": "object",
                                "properties": {
                                    "x": { "type": "number" }, "y": { "type": "number" },
                                    "width": { "type": "number" }, "height": { "type": "number" }
                                }
                            }
                        }
                    }
                },
                "truncated": { "type": "boolean" }
            }
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(DEFAULT_LIMIT);
        let limit = limit.clamp(1, MAX_LIMIT);
        let body = ELEMENTS_SCRIPT.replace("__LIMIT__", &limit.to_string());
        let answer =
            self.browser.execute_script(&crate::viewport::script("elements", &body)).await?;
        if !answer.get("elements").is_some_and(Value::is_array) {
            return Err(adk_core::AdkError::tool(
                "Error: the page did not return its elements; reload it and call \
                 browser_elements again",
            ));
        }
        Ok(answer)
    }
}
