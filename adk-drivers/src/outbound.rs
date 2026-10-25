//! The rules every outbound message keeps, whichever driver sends it (spec 3.5 §7.10,
//! §7.13, A-I8, A-I17).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use adk_core::{Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{DriverError, Result};

/// The origin every in-process driver tool reports: `adk-drivers:<class>`.
pub const IN_PROCESS_ORIGIN_PREFIX: &str = "adk-drivers:";

/// The line every outbound message carries (spec 3.5 A-I17, §13 Q6): it names the site's
/// owner and says an AI agent wrote the message.
///
/// One default line, overridable per jurisdiction, never removable: an override must
/// still name the owner and say "AI", and every driver's outbox appends the line to every
/// message it sends. [`apply`](Self::apply) is idempotent, so a draft that already ends
/// with the line keeps one copy.
///
/// # Example
///
/// ```rust
/// use adk_drivers::Disclosure;
///
/// let disclosure = Disclosure::new("Wanjiru's Bakery").unwrap();
/// let body = disclosure.apply("Your order is ready.");
/// assert_eq!(body, "Your order is ready.\n\nThis message was written by an AI agent for Wanjiru's Bakery.");
/// assert_eq!(disclosure.apply(&body), body);
/// assert!(Disclosure::new("Wanjiru's Bakery").unwrap().with_line("Sent by a bot").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Disclosure {
    owner: String,
    line: String,
}

impl Disclosure {
    /// The default line for `owner`: "This message was written by an AI agent for {owner}."
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for an empty owner or one with a line break.
    pub fn new(owner: impl Into<String>) -> Result<Self> {
        let owner = owner.into().trim().to_string();
        if owner.is_empty() || owner.contains(['\r', '\n']) {
            return Err(DriverError::invalid(
                "the disclosure names the site's owner: give their name on one line",
            ));
        }
        let line = format!("This message was written by an AI agent for {owner}.");
        Ok(Self { owner, line })
    }

    /// Replaces the line, as a jurisdiction's wording requires.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] when the line does not name the owner, does not say
    /// "AI", or spans more than one line: the disclosure can be reworded, never removed.
    pub fn with_line(mut self, line: impl Into<String>) -> Result<Self> {
        let line = line.into().trim().to_string();
        let says_ai = line
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| word == "AI" || word.eq_ignore_ascii_case("KI") || word == "IA");
        if line.contains(['\r', '\n']) || !line.contains(&self.owner) || !says_ai {
            return Err(DriverError::invalid(format!(
                "a disclosure line names the owner ({}) and says an AI agent wrote the message, \
                 on one line (spec 3.5 A-I17)",
                self.owner
            )));
        }
        self.line = line;
        Ok(self)
    }

    /// The owner the line names.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The line.
    pub fn line(&self) -> &str {
        &self.line
    }

    /// `body` ending with the line, after a blank line; unchanged when it already ends
    /// with it.
    pub fn apply(&self, body: &str) -> String {
        let body = body.trim_end();
        if body.ends_with(&self.line) {
            body.to_string()
        } else if body.is_empty() {
            self.line.clone()
        } else {
            format!("{body}\n\n{}", self.line)
        }
    }

    /// Whether `body` ends with the line.
    pub fn is_present(&self, body: &str) -> bool {
        body.trim_end().ends_with(&self.line)
    }
}

/// What an unsubscribe or a revocation stops (spec 3.5 A-I17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum SuppressionScope {
    /// Sequences, newsletters and posts addressed to them: an unsubscribe.
    Marketing,
    /// Every message, replies included: a revocation, or "stop contacting me".
    All,
}

/// A recipient who unsubscribed or revoked (spec 3.5 A-I17).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Suppression {
    /// The recipient, as [`recipient_key`] writes it.
    pub recipient: String,
    /// What it stops.
    pub scope: SuppressionScope,
    /// When.
    pub at: DateTime<Utc>,
    /// Where it came from: `link`, `mailto`, `complaint`, `owner`.
    pub source: String,
}

impl Suppression {
    /// `recipient` suppressed for `scope` now, from `source`.
    pub fn new(recipient: &str, scope: SuppressionScope, source: impl Into<String>) -> Self {
        Self { recipient: recipient_key(recipient), scope, at: Utc::now(), source: source.into() }
    }
}

/// A recipient as suppressions and consent key it: trimmed and in lower case.
pub fn recipient_key(recipient: &str) -> String {
    recipient.trim().to_lowercase()
}

/// Where unsubscribes and revocations are kept (spec 3.5 A-I17): every driver's outbox
/// reads it before each send, so one takes effect before the next send.
#[async_trait]
pub trait SuppressionList: Send + Sync {
    /// Keeps `suppression`; a wider scope replaces a narrower one, never the reverse.
    async fn suppress(&self, suppression: Suppression) -> Result<()>;

    /// The suppression of `recipient`, if any.
    async fn lookup(&self, recipient: &str) -> Result<Option<Suppression>>;

    /// Lifts the suppression of `recipient`, when they subscribe again with a fresh
    /// consent; returns whether there was one.
    async fn lift(&self, recipient: &str) -> Result<bool>;
}

/// Suppressions in process memory, for tests and single-process hosts.
#[derive(Debug, Default)]
pub struct InMemorySuppressions {
    entries: Mutex<BTreeMap<String, Suppression>>,
}

impl InMemorySuppressions {
    /// None suppressed.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SuppressionList for InMemorySuppressions {
    async fn suppress(&self, suppression: Suppression) -> Result<()> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let key = recipient_key(&suppression.recipient);
        let wider = entries.get(&key).is_none_or(|known| suppression.scope >= known.scope);
        if wider {
            entries.insert(key, suppression);
        }
        Ok(())
    }

    async fn lookup(&self, recipient: &str) -> Result<Option<Suppression>> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(entries.get(&recipient_key(recipient)).cloned())
    }

    async fn lift(&self, recipient: &str) -> Result<bool> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(entries.remove(&recipient_key(recipient)).is_some())
    }
}

/// Whether a recipient holds a live consent receipt for a scope (spec 3.5 A-I8): the edge's
/// consent store answers it; sequences and newsletters ask it at send time.
#[async_trait]
pub trait ConsentCheck: Send + Sync {
    /// Whether `recipient` holds a live receipt for `scope`.
    async fn is_live(&self, recipient: &str, scope: &str) -> Result<bool>;
}

/// Consent receipts in process memory, for tests.
///
/// # Example
///
/// ```rust
/// use adk_drivers::{ConsentCheck, InMemoryConsent};
///
/// # futures::executor::block_on(async {
/// let consent = InMemoryConsent::new();
/// consent.grant("ana@example.com", "newsletter");
/// assert!(consent.is_live("Ana@Example.com", "newsletter").await.unwrap());
/// consent.revoke("ana@example.com", "newsletter");
/// assert!(!consent.is_live("ana@example.com", "newsletter").await.unwrap());
/// # });
/// ```
#[derive(Debug, Default)]
pub struct InMemoryConsent {
    granted: Mutex<BTreeSet<(String, String)>>,
}

impl InMemoryConsent {
    /// No consent given.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `recipient`'s consent for `scope`.
    pub fn grant(&self, recipient: &str, scope: &str) {
        self.granted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((recipient_key(recipient), scope.to_string()));
    }

    /// Revokes it.
    pub fn revoke(&self, recipient: &str, scope: &str) {
        self.granted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(recipient_key(recipient), scope.to_string()));
    }
}

#[async_trait]
impl ConsentCheck for InMemoryConsent {
    async fn is_live(&self, recipient: &str, scope: &str) -> Result<bool> {
        Ok(self
            .granted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&(recipient_key(recipient), scope.to_string())))
    }
}

/// At most `max` calls per `per` (spec 3.5 §7.10: `reply_in_thread` is rate limited).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RateLimit {
    /// Calls allowed in a window.
    pub max: u32,
    /// The window.
    pub per: Duration,
}

impl RateLimit {
    /// `max` calls per `per`.
    pub fn new(max: u32, per: Duration) -> Self {
        Self { max, per }
    }
}

/// A sliding-window limiter keyed by string.
#[derive(Debug)]
pub(crate) struct Limiter {
    limit: RateLimit,
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Limiter {
    pub(crate) fn new(limit: RateLimit) -> Self {
        Self { limit, hits: Mutex::default() }
    }

    /// Takes one call for `key`, or says how long until one is free.
    pub(crate) fn take(&self, key: &str, what: &str) -> Result<()> {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap_or_else(PoisonError::into_inner);
        let window = hits.entry(key.to_string()).or_default();
        while window.front().is_some_and(|at| now.duration_since(*at) >= self.limit.per) {
            window.pop_front();
        }
        if window.len() >= self.limit.max as usize {
            let retry = window
                .front()
                .map(|at| self.limit.per.saturating_sub(now.duration_since(*at)))
                .unwrap_or(self.limit.per);
            return Err(DriverError::RateLimited {
                reason: format!(
                    "{what}: at most {} per {} s",
                    self.limit.max,
                    self.limit.per.as_secs()
                ),
                retry_after_secs: retry.as_secs().max(1),
            });
        }
        window.push_back(now);
        Ok(())
    }
}

/// The playbook's reply templates (spec 3.5 §7.10: `reply_in_thread` answers "within a
/// playbook template"): a public agent fills a template's `{fields}`, never writes a
/// message from nothing.
///
/// # Example
///
/// ```rust
/// use std::collections::BTreeMap;
/// use adk_drivers::ReplyTemplates;
///
/// let templates = ReplyTemplates::new().template("hours", "Hi {name}, we open at {time}.");
/// let fields = BTreeMap::from([("name".to_string(), "Ana".to_string()), ("time".to_string(), "8am".to_string())]);
/// assert_eq!(templates.render("hours", &fields).unwrap(), "Hi Ana, we open at 8am.");
/// assert!(templates.render("refund", &fields).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplyTemplates {
    templates: BTreeMap<String, String>,
    max_field_chars: usize,
}

impl Default for ReplyTemplates {
    fn default() -> Self {
        Self { templates: BTreeMap::new(), max_field_chars: 1_500 }
    }
}

impl ReplyTemplates {
    /// No templates; fields of at most 1,500 characters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds template `name`, whose `{field}` placeholders the agent fills.
    #[must_use]
    pub fn template(mut self, name: impl Into<String>, text: impl Into<String>) -> Self {
        self.templates.insert(name.into(), text.into());
        self
    }

    /// Sets the longest field value.
    #[must_use]
    pub fn max_field_chars(mut self, chars: usize) -> Self {
        self.max_field_chars = chars;
        self
    }

    /// The template names.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.templates.keys().map(String::as_str)
    }

    /// Template `name` with `fields` filled in.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Refused`] for a template the playbook does not have, and
    /// [`DriverError::Invalid`] for a missing, unknown or too long field.
    pub fn render(&self, name: &str, fields: &BTreeMap<String, String>) -> Result<String> {
        let Some(template) = self.templates.get(name) else {
            let names: Vec<&str> = self.names().collect();
            return Err(DriverError::refused(format!(
                "`{name}` is not one of the playbook's reply templates ({})",
                names.join(", ")
            )));
        };
        let placeholders = placeholders(template);
        if let Some(extra) = fields.keys().find(|field| !placeholders.contains(field.as_str())) {
            return Err(DriverError::invalid(format!("template `{name}` has no field `{extra}`")));
        }
        let mut out = template.clone();
        for placeholder in &placeholders {
            let Some(value) = fields.get(*placeholder) else {
                return Err(DriverError::invalid(format!(
                    "template `{name}` needs field `{placeholder}`"
                )));
            };
            if value.chars().count() > self.max_field_chars {
                return Err(DriverError::invalid(format!(
                    "field `{placeholder}` is longer than {} characters",
                    self.max_field_chars
                )));
            }
            out = out.replace(&format!("{{{placeholder}}}"), value);
        }
        Ok(out)
    }
}

/// The `{name}` placeholders of `template`.
fn placeholders(template: &str) -> BTreeSet<&str> {
    let mut out = BTreeSet::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else { break };
        let name = &after[..end];
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            out.insert(name);
        }
        rest = &after[end + 1..];
    }
    out
}

/// A driver that answers the person who wrote, in the same thread (spec 3.5 §7.10): the
/// email and social outboxes.
#[async_trait]
pub trait ThreadReplier: Send + Sync {
    /// The prefix of the thread references it answers: `email` or `social`.
    fn prefix(&self) -> &str;

    /// Answers thread `thread` (without the prefix) with template `template` filled with
    /// `fields`, under the rules of `reply_in_thread`.
    async fn reply_in_thread(
        &self,
        thread: &str,
        template: &str,
        fields: &BTreeMap<String, String>,
    ) -> Result<Value>;
}

/// `reply_in_thread`, the public partition's only `SendExternal` tool (spec 3.5 §7.10,
/// A-I2): answers the person who wrote, in their thread, within a playbook template, at a
/// rate limit, with no new recipients and no attachments.
///
/// A thread reference is `<prefix>:<thread>`, as the inbox gave it: `email:...` or
/// `social:...`; each prefix goes to its [`ThreadReplier`].
pub struct ReplyInThread {
    repliers: Vec<Arc<dyn ThreadReplier>>,
}

impl std::fmt::Debug for ReplyInThread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prefixes: Vec<&str> = self.repliers.iter().map(|replier| replier.prefix()).collect();
        f.debug_struct("ReplyInThread").field("prefixes", &prefixes).finish()
    }
}

impl ReplyInThread {
    /// The tool over `repliers`.
    pub fn new(repliers: impl IntoIterator<Item = Arc<dyn ThreadReplier>>) -> Self {
        Self { repliers: repliers.into_iter().collect() }
    }

    /// Answers `thread_ref` (`<prefix>:<thread>`).
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a reference no replier serves, and the
    /// replier's error.
    pub async fn reply(
        &self,
        thread_ref: &str,
        template: &str,
        fields: &BTreeMap<String, String>,
    ) -> Result<Value> {
        let (prefix, thread) = thread_ref.split_once(':').ok_or_else(|| {
            DriverError::invalid(format!(
                "`{thread_ref}` is not a thread reference; use the `thread` the inbox gave"
            ))
        })?;
        let replier =
            self.repliers.iter().find(|replier| replier.prefix() == prefix).ok_or_else(|| {
                DriverError::invalid(format!("no driver answers `{prefix}` threads"))
            })?;
        replier.reply_in_thread(thread, template, fields).await
    }
}

#[async_trait]
impl Tool for ReplyInThread {
    fn name(&self) -> &str {
        "reply_in_thread"
    }

    fn description(&self) -> &str {
        "Answer the person who wrote, in the same thread, by filling one of the playbook's \
         reply templates. Goes only to the thread's sender; no new recipients, no attachments."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "thread": {"type": "string", "description": "The thread reference the inbox gave, such as email:... or social:..."},
                "template": {"type": "string", "description": "The playbook's reply template"},
                "fields": {"type": "object", "additionalProperties": {"type": "string"}, "description": "The template's fields"}
            },
            "required": ["thread", "template", "fields"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    fn origin(&self) -> &str {
        "adk-drivers:reply"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let thread = str_arg(&args, "thread")?;
        let template = str_arg(&args, "template")?;
        let fields = fields_arg(&args)?;
        Ok(self.reply(&thread, &template, &fields).await?)
    }
}

/// The string argument `name`.
pub(crate) fn str_arg(args: &Value, name: &str) -> Result<String> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| DriverError::invalid(format!("`{name}` is required, as a string")))
}

/// The `fields` object of a template call.
pub(crate) fn fields_arg(args: &Value) -> Result<BTreeMap<String, String>> {
    let Some(fields) = args.get("fields") else {
        return Ok(BTreeMap::new());
    };
    let object = fields
        .as_object()
        .ok_or_else(|| DriverError::invalid("`fields` is an object of strings"))?;
    object
        .iter()
        .map(|(name, value)| match value {
            Value::String(text) => Ok((name.clone(), text.clone())),
            Value::Number(number) => Ok((name.clone(), number.to_string())),
            _ => Err(DriverError::invalid(format!("field `{name}` is not a string"))),
        })
        .collect()
}

/// `text` cut to at most `max` characters, with `…` where it was cut.
pub(crate) fn bounded(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}
