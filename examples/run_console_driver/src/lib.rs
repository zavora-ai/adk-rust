//! The lifecycle an agent reporting into the run console has to get right.
//!
//! Two examples drive that console, and every correctness fix landed in one of them.
//! The other kept the original behaviour: no cancellation monitoring, no reconciliation
//! when a model stops without saying so, no credential redaction, and a failure check
//! that flagged successful calls. A review found the same items under "both consoles"
//! and only one console had them. That is the argument for this crate — not tidiness,
//! but that a lifecycle fix applied once should not have to be remembered twice.
//!
//! What belongs here: polling the console, tracking which of the person's messages have
//! been answered, noticing a stop, bounding a turn, redacting secrets before anything is
//! logged, deciding whether a tool result actually failed, and closing a turn honestly.
//!
//! What does not: anything about Blender, dashboards or metrics. Domain routing stays in
//! each agent's own brief and skill, where it can be read and changed without touching
//! the machinery.

use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;

/// Field names whose values must never be printed or sent to the console.
///
/// An agent may be given a sign-in deliberately, which puts a password in the model's
/// context — a decided trade. The same string travelling on to a terminal and to the
/// activity feed a person is watching is not, because that is where it lands in a log
/// file or a screenshot. Measured before this existed: a form-fill call printed its
/// fields verbatim, and the password survived only because another field sorted first
/// and the preview happened to cut at 110 characters. A secret protected by truncation
/// is not protected.
const SECRET_FIELD: &[&str] = &[
    "password", "passwd", "pwd", "secret", "token", "api_key", "apikey", "credential",
    "authorization", "auth", "session", "cookie", "otp", "passcode", "pin",
];

fn is_secret_name(name: &str) -> bool {
    let lowered = name.to_lowercase();
    SECRET_FIELD.iter().any(|secret| lowered.contains(secret))
}

/// Exact secret values to scrub, wherever they appear.
///
/// Name-based redaction has a hole it cannot close. When an agent types a password it
/// does so through a generic text tool, whose argument is `{"text": "…"}` — no key and
/// no sibling says "password", so nothing marks it. The same is true in reverse for tool
/// *results*, which may echo back what was typed or return a page containing a token.
///
/// A driver that was handed a credential knows the string, so it can scrub that string
/// from anything on its way out. That is precise: no heuristic, no false positives, and
/// it covers both holes with one rule.
static KNOWN_SECRETS: std::sync::LazyLock<std::sync::RwLock<Vec<String>>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(Vec::new()));

/// Register a value that must never appear in a log or the console.
///
/// Call this for every credential handed to the agent, at startup. Short values are
/// ignored: scrubbing a two-character string would redact half of every message, and a
/// secret that short is not protected by redaction anyway.
pub fn register_secret(value: &str) {
    let value = value.trim();
    if value.len() < 6 {
        return;
    }
    let mut secrets = KNOWN_SECRETS.write().expect("secrets lock");
    if !secrets.iter().any(|held| held == value) {
        secrets.push(value.to_string());
    }
}

/// Replace every registered secret value with a marker.
///
/// Applied to arguments and results alike, and to the whole serialized string rather
/// than to parsed fields, because a secret can be nested, concatenated or echoed inside
/// text that is not JSON at all.
pub fn scrub_known_secrets(text: &str) -> String {
    let secrets = KNOWN_SECRETS.read().expect("secrets lock");
    if secrets.is_empty() {
        return text.to_string();
    }
    let mut out = text.to_string();
    for secret in secrets.iter() {
        if out.contains(secret.as_str()) {
            out = out.replace(secret.as_str(), "[redacted]");
        }
    }
    out
}

/// Everything that must happen to a tool argument before it is shown.
///
/// Both halves are needed. The name rules catch a credential in a field that announces
/// itself, including one recognised by a sibling key, which is how form fields carry
/// their name and value separately. The value rules catch the same secret arriving
/// through a field that announces nothing.
pub fn redact_for_display(json: &str) -> String {
    scrub_known_secrets(&redact_secrets(json))
}

/// Replace secret-shaped values anywhere in a serialized tool argument.
///
/// Textual rather than typed on purpose: this runs over an already-serialized argument
/// whose shape differs per tool and per server, and a redactor that understood only one
/// shape would miss the next. It errs towards redacting.
pub fn redact_secrets(json: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(json) else {
        // Unparseable, so its shape is unknown and anything could be in it.
        let lowered = json.to_lowercase();
        return if SECRET_FIELD.iter().any(|name| lowered.contains(name)) {
            "[redacted: unparseable arguments naming a credential field]".to_string()
        } else {
            json.to_string()
        };
    };
    redact_value(&mut value);
    serde_json::to_string(&value).unwrap_or_else(|_| "[redacted]".to_string())
}

fn redact_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            // A form field carries its name in one key and its value in another, so a
            // secret is often recognised by a sibling rather than by its own key.
            let names: Vec<String> = map.keys().cloned().collect();
            let sibling_names_a_secret = names.iter().any(|key| {
                map.get(key).and_then(Value::as_str).is_some_and(is_secret_name)
            });
            for key in names {
                let key_is_secret = is_secret_name(&key);
                if let Some(entry) = map.get_mut(&key) {
                    if key_is_secret || (sibling_names_a_secret && key == "value") {
                        *entry = Value::String("[redacted]".into());
                    } else {
                        redact_value(entry);
                    }
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_value),
        _ => {}
    }
}

/// Did this tool result actually fail?
///
/// MCP reports failure in a structured field. Searching the serialized body for the
/// text `isError` matched `"isError": false` too, so successful calls were reported as
/// failures in the feed the person watches. A domain error carried inside the returned
/// content still counts, because that is how these servers report one — but a null or
/// false `error` field is the success it looks like, and treating it as a failure would
/// mark almost everything red.
pub fn response_failed(response: &Value) -> bool {
    if response.get("isError").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    let has_error = |value: &Value| {
        value
            .get("error")
            .is_some_and(|error| !error.is_null() && error.as_bool() != Some(false))
    };
    if has_error(response) {
        return true;
    }
    response.get("content").and_then(Value::as_array).is_some_and(|parts| {
        parts.iter().any(|part| {
            part.get("text")
                .and_then(Value::as_str)
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .is_some_and(|parsed| has_error(&parsed))
        })
    })
}

/// Truncate for a one-line feed, marking that something was cut.
pub fn preview(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() { format!("{head}…") } else { head }
}

/// Condense JSON into something readable in a one-line activity feed.
pub fn summarise(json: &str, max_chars: usize) -> String {
    let cleaned: String = json
        .trim_start_matches('{')
        .trim_end_matches('}')
        .replace("\\n", " ")
        .replace(['"', '{', '}'], "")
        .replace(':', ": ")
        .replace(',', ", ");
    preview(&cleaned.split_whitespace().collect::<Vec<_>>().join(" "), max_chars)
}

/// Optional bounds on a single turn.
///
/// Unset by default, deliberately. Thinking is uncapped because a capped reasoning
/// budget produced worse answers, and a token limit that truncates a thought spends the
/// tokens without buying the conclusion. The point of a bound is not to save money but
/// to know why something stopped, so when one is hit it is reported rather than looking
/// like a finish.
#[derive(Debug, Clone, Copy, Default)]
pub struct Budget {
    pub seconds: Option<u64>,
    /// Tool calls, which is the better proxy for cost here: each call carries its result
    /// back into context, and it is something a driver can actually count.
    pub calls: Option<u64>,
}

impl Budget {
    /// Read `<PREFIX>_MAX_SECONDS` and `<PREFIX>_MAX_TOOL_CALLS`, announcing what applies.
    pub fn from_env(prefix: &str) -> Self {
        let read = |suffix: &str| {
            std::env::var(format!("{prefix}_{suffix}"))
                .ok()
                .and_then(|raw| raw.parse::<u64>().ok())
                .filter(|value| *value > 0)
        };
        let budget = Self { seconds: read("MAX_SECONDS"), calls: read("MAX_TOOL_CALLS") };
        match (budget.seconds, budget.calls) {
            (None, None) => println!("  · no turn budget — thinking is uncapped"),
            (seconds, calls) => println!(
                "  ✓ turn budget: {} {}",
                seconds.map_or("no time limit".to_string(), |s| format!("{s}s")),
                calls.map_or("no call limit".to_string(), |c| format!("/ {c} calls")),
            ),
        }
        budget
    }

    /// Why this turn should stop, if it should. The reason names the budget and says
    /// what had been done, because a bound that stops work silently is worse than none.
    pub fn exceeded(&self, started: Instant, calls: u64) -> Option<String> {
        if let Some(limit) = self.seconds
            && started.elapsed().as_secs() >= limit
        {
            return Some(format!(
                "the {limit}s time budget for one turn was reached after {calls} tool calls"
            ));
        }
        if let Some(limit) = self.calls
            && calls >= limit
        {
            return Some(format!(
                "the {limit}-call budget for one turn was reached after {}s",
                started.elapsed().as_secs()
            ));
        }
        None
    }
}

/// Expose only an allowed subset of another toolset's tools.
///
/// `autoApprove` in an MCP server config does not do this. It is parsed into
/// `auto_approve` and never consulted when tools are listed, so an agent configured
/// with it still receives the server's whole surface. Approval configuration is not
/// access control.
pub struct Allowed {
    pub inner: Arc<dyn adk_core::Toolset>,
    /// Lists whose tools may pass. Separate lists so each server's surface is stated
    /// once, rather than merged by hand into a copy that drifts.
    pub allow: Vec<&'static [&'static str]>,
    /// Refused whatever else allows them. Asserted rather than assumed, because a
    /// withheld tool appearing is a capability leak, not a cosmetic slip.
    pub withheld: Vec<&'static str>,
    /// Tools passed through by name prefix, for a server whose whole surface is needed.
    /// Empty means no passthrough — an empty prefix would allow everything.
    pub passthrough_prefix: Option<&'static str>,
}

impl Allowed {
    pub fn permits(&self, name: &str) -> bool {
        // Names are prefixed `server__tool` only when they collide across servers, so
        // match the trailing segment.
        let leaf = name.rsplit("__").next().unwrap_or(name);
        if self.withheld.contains(&leaf) {
            return false;
        }
        if let Some(prefix) = self.passthrough_prefix
            && !prefix.is_empty()
            && leaf.starts_with(prefix)
        {
            return true;
        }
        self.allow.iter().any(|list| list.contains(&leaf))
    }
}

#[async_trait::async_trait]
impl adk_core::Toolset for Allowed {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn tools(
        &self,
        ctx: Arc<dyn adk_core::ReadonlyContext>,
    ) -> adk_core::Result<Vec<Arc<dyn adk_core::Tool>>> {
        Ok(self
            .inner
            .tools(ctx)
            .await?
            .into_iter()
            .filter(|tool| self.permits(tool.name()))
            .collect())
    }
}

/// How a turn ended. Distinguished because a person cannot otherwise tell a finish from
/// a hang, a stop or a bound being reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The model finished and reported a conclusion.
    Completed,
    /// The model stopped without reporting one.
    Incomplete,
    /// A mid-stream error.
    Failed(String),
    /// The person asked it to stop.
    Cancelled,
    /// A budget was reached; the string names which.
    OutOfBudget(String),
}

/// A read-and-report view of the console host.
pub struct Console {
    base: String,
    http: reqwest::Client,
}

impl Console {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into().trim_end_matches('/').to_string(), http: reqwest::Client::new() }
    }

    async fn page_call(&self, name: &str) -> Option<Value> {
        let response = self
            .http
            .post(format!("{}/rpc", self.base))
            .json(&serde_json::json!({ "name": name, "arguments": {} }))
            .send()
            .await
            .ok()?;
        let body: Value = response.json().await.ok()?;
        let text = body.get("content")?.get(0)?.get("text")?.as_str()?;
        serde_json::from_str(text).ok()
    }

    pub async fn run_id(&self) -> Option<String> {
        let text = self
            .http
            .get(format!("{}/run-id", self.base))
            .send()
            .await
            .ok()?
            .text()
            .await
            .ok()?;
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    pub async fn read(&self) -> Option<Value> {
        self.page_call("run_console").await
    }

    /// Messages the person sent that have not been acknowledged.
    ///
    /// Read from the console rather than inferred from the transcript. The original rule
    /// was "the last message is the person's", which the agent's own narration
    /// falsified: a progress narration appends an agent message, so a question typed
    /// mid-run stopped being last and became invisible.
    pub async fn pending(&self) -> Vec<Value> {
        self.read()
            .await
            .and_then(|run| run.get("pending").and_then(Value::as_array).cloned())
            .unwrap_or_default()
    }

    // Acknowledging is the **agent's** job, not the driver's, and deliberately so.
    //
    // The page endpoint pins `role: "user"` and drops the `acknowledge` argument — it
    // exists for a person typing, so a driver posting through it would append another
    // user message and create a new pending item rather than clearing one. The agent
    // already holds `run_say`, which takes `acknowledge`, so it says what it answered
    // as part of answering. The brief tells it to.

    /// Has the person asked the agent to stop?
    ///
    /// Worth reading between stream events rather than only when the model calls a run
    /// tool: an agent deep in a thought may not call one for some time, and a console
    /// reading "Stopping…" while tokens are still being spent is what a stop button
    /// exists to prevent.
    pub async fn cancel_requested(&self) -> bool {
        self.read()
            .await
            .and_then(|run| run.get("cancelRequested").and_then(Value::as_bool))
            .unwrap_or(false)
    }

    async fn post_driver(&self, body: Value) {
        let _ = self.http.post(format!("{}/driver", self.base)).json(&body).send().await;
    }

    pub async fn report_activity(&self, events: &[Value]) {
        if events.is_empty() {
            return;
        }
        let Some(run_id) = self.run_id().await else { return };
        let _ = self
            .http
            .post(format!("{}/driver/activity", self.base))
            .json(&serde_json::json!({ "runId": run_id, "events": events }))
            .await_send()
            .await;
    }

    /// Report what a model call cost, so the run's price is visible as it accrues.
    pub async fn report_usage(&self, usage: Value) {
        let Some(run_id) = self.run_id().await else { return };
        let mut body = usage;
        if let Some(map) = body.as_object_mut() {
            map.insert("runId".into(), Value::String(run_id));
        }
        let _ = self.http.post(format!("{}/driver/usage", self.base)).json(&body).send().await;
    }

    /// Close a turn, saying honestly how it ended.
    ///
    /// A model can stop without ever reporting a terminal state, which leaves the
    /// console reading "working" — indistinguishable, on screen, from a hang. The driver
    /// knows the turn ended, so it says so.
    pub async fn finish(&self, outcome: &Outcome, closing: &str) {
        let Some(run_id) = self.run_id().await else { return };
        // Do not overwrite a terminal state the agent set itself.
        if let Some(state) = self.read().await.and_then(|run| {
            run.get("state").and_then(Value::as_str).map(str::to_string)
        }) && (state == "done" || state == "failed")
            && !matches!(outcome, Outcome::Cancelled | Outcome::OutOfBudget(_))
        {
            return;
        }
        let head = |limit: usize| closing.chars().take(limit).collect::<String>();
        let (state, narration) = match outcome {
            Outcome::Completed => ("done", head(600)),
            Outcome::Incomplete => (
                "failed",
                "The turn ended without a closing summary. Nothing further was reported."
                    .to_string(),
            ),
            Outcome::Failed(message) => ("failed", format!("The turn failed: {message}")),
            Outcome::Cancelled if closing.is_empty() => (
                "done",
                "Stopped at your request. Nothing had been concluded yet.".to_string(),
            ),
            Outcome::Cancelled => (
                "done",
                format!("Stopped at your request. What I had so far: {}", head(500)),
            ),
            Outcome::OutOfBudget(reason) if closing.is_empty() => (
                "failed",
                format!("Stopped before finishing: {reason}. Nothing had been concluded yet."),
            ),
            Outcome::OutOfBudget(reason) => (
                "failed",
                format!("Stopped before finishing: {reason}. What I had so far: {}", head(500)),
            ),
        };
        self.post_driver(serde_json::json!({
            "runId": run_id, "state": state, "narration": narration,
        }))
        .await;
    }
}

/// Small shim so `report_activity` reads the same as the others.
trait AwaitSend {
    async fn await_send(self) -> reqwest::Result<reqwest::Response>;
}

impl AwaitSend for reqwest::RequestBuilder {
    async fn await_send(self) -> reqwest::Result<reqwest::Response> {
        self.send().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_fill_does_not_leak_the_password() {
        // The exact shape observed leaking, from a real run.
        let args = r#"{"fields":[
            {"name":"Email address","value":"admin@example.invalid"},
            {"name":"Password","value":"Analytics123!"}
        ]}"#;
        let safe = redact_secrets(args);
        assert!(!safe.contains("Analytics123!"), "{safe}");
        assert!(safe.contains("admin@example.invalid"), "a username is not a secret");
    }

    #[test]
    fn a_secret_named_key_is_redacted_wherever_it_sits() {
        for args in [
            r#"{"password":"hunter2"}"#,
            r#"{"nested":{"api_key":"hunter2"}}"#,
            r#"{"list":[{"token":"hunter2"}]}"#,
        ] {
            assert!(!redact_secrets(args).contains("hunter2"), "leaked from {args}");
        }
    }

    #[test]
    fn ordinary_arguments_stay_readable() {
        // Over-redacting makes the feed useless, which is its own failure.
        let safe = redact_secrets(r#"{"chart_id":"37","value":"Trains"}"#);
        assert!(safe.contains("Trains"));
        assert!(!safe.contains("[redacted]"));
    }

    #[test]
    fn success_is_not_reported_as_failure() {
        assert!(!response_failed(&serde_json::json!({ "isError": false })));
        assert!(!response_failed(&serde_json::json!({ "error": Value::Null })));
        assert!(!response_failed(&serde_json::json!({ "error": false })));
        assert!(response_failed(&serde_json::json!({ "isError": true })));
        assert!(response_failed(&serde_json::json!({ "error": "boom" })));
    }

    #[test]
    fn a_domain_error_inside_the_content_still_counts() {
        let response = serde_json::json!({
            "content": [{ "text": "{\"error\": \"bi_error\", \"message\": \"HTTP 500\"}" }]
        });
        assert!(response_failed(&response));
    }

    #[test]
    fn no_budget_never_stops_a_turn() {
        assert!(Budget::default().exceeded(Instant::now(), 10_000).is_none());
    }

    #[test]
    fn a_budget_names_itself_and_what_was_done() {
        let calls = Budget { seconds: None, calls: Some(5) };
        let reason = calls.exceeded(Instant::now(), 5).expect("should stop");
        assert!(reason.contains("5-call"), "{reason}");

        let time = Budget { seconds: Some(1), calls: None };
        let long_ago = Instant::now() - std::time::Duration::from_secs(2);
        let reason = time.exceeded(long_ago, 7).expect("should stop");
        assert!(reason.contains("1s time budget") && reason.contains("7 tool calls"), "{reason}");
    }

    struct NoTools;

    #[async_trait::async_trait]
    impl adk_core::Toolset for NoTools {
        fn name(&self) -> &str {
            "none"
        }
        async fn tools(
            &self,
            _ctx: Arc<dyn adk_core::ReadonlyContext>,
        ) -> adk_core::Result<Vec<Arc<dyn adk_core::Tool>>> {
            Ok(Vec::new())
        }
    }

    fn filter(allow: Vec<&'static [&'static str]>, withheld: Vec<&'static str>, prefix: Option<&'static str>) -> Allowed {
        Allowed { inner: Arc::new(NoTools), allow, withheld, passthrough_prefix: prefix }
    }

    #[test]
    fn withheld_beats_allowed() {
        const LIST: &[&str] = &["screenshot", "run_script"];
        let f = filter(vec![LIST], vec!["run_script"], None);
        assert!(f.permits("screenshot"));
        assert!(!f.permits("run_script"), "a withheld tool must not pass even if listed");
    }

    #[test]
    fn a_prefix_passthrough_is_not_a_wildcard() {
        const LIST: &[&str] = &["screenshot"];
        let f = filter(vec![LIST], vec![], Some("blender"));
        assert!(f.permits("blender_execute_code"));
        assert!(!f.permits("filesystem"), "an unlisted tool must not pass");
        // An empty prefix would allow everything, so it is treated as no passthrough.
        let empty = filter(vec![LIST], vec![], Some(""));
        assert!(!empty.permits("filesystem"));
    }

    #[test]
    fn a_collision_prefixed_name_is_matched_by_its_leaf() {
        const LIST: &[&str] = &["browser_click"];
        let f = filter(vec![LIST], vec![], None);
        assert!(f.permits("browser__browser_click"), "server__tool must match on the tool");
    }
}

#[cfg(test)]
mod value_redaction_tests {
    use super::*;

    #[test]
    fn a_secret_typed_through_a_generic_text_field_is_still_scrubbed() {
        // The hole name-based redaction cannot close: `type` takes `{"text": "…"}`, and
        // nothing about that field says it holds a password.
        register_secret("Analytics123!");
        let args = r#"{"target":"e42","text":"Analytics123!"}"#;
        assert!(!redact_for_display(args).contains("Analytics123!"));
        // And the name-based half still works for a field that does announce itself.
        assert!(!redact_for_display(r#"{"password":"whatever-long-enough"}"#).contains("whatever-long-enough"));
    }

    #[test]
    fn a_secret_echoed_back_in_a_result_is_scrubbed_too() {
        // A result can echo what was typed, or return a page containing a token.
        register_secret("s3cret-token-value");
        let body = r#"{"content":[{"text":"Authorization: Bearer s3cret-token-value"}]}"#;
        assert!(!redact_for_display(body).contains("s3cret-token-value"));
    }

    #[test]
    fn a_secret_is_scrubbed_from_text_that_is_not_json_at_all() {
        register_secret("plaintext-secret-here");
        assert!(!scrub_known_secrets("typed plaintext-secret-here into the field")
            .contains("plaintext-secret-here"));
    }

    #[test]
    fn a_very_short_value_is_not_registered() {
        // Scrubbing "ab" would redact half of every message, and a secret that short is
        // not protected by redaction anyway.
        register_secret("abc");
        assert_eq!(scrub_known_secrets("abc def"), "abc def");
    }

    #[test]
    fn ordinary_text_is_untouched_when_nothing_matches() {
        let ordinary = r#"{"chart_id":"37","value":"Trains"}"#;
        assert!(redact_for_display(ordinary).contains("Trains"));
    }
}
