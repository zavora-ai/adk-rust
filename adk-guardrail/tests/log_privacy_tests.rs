//! Guardrail reasons stay out of logs.
//!
//! A tool guardrail's denial reason quotes the argument it refused, such as a path, and a
//! transform reason can quote the content it replaced. Both were logged, at WARN and DEBUG,
//! whatever the run's `RunConfig::record_payloads`. The reasons still reach the caller.

use std::sync::{Arc, Mutex};

use adk_core::Content;
use adk_guardrail::{
    Guardrail, GuardrailExecutor, GuardrailResult, GuardrailSet, PathAllowList, Severity,
    ToolCallDecision, ToolGuardrail, ToolGuardrailResult, ToolGuardrailSet, ToolResultDecision,
    ToolResultGuardrailResult,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

const SECRET: &str = "acct-7731-secret";

#[derive(Clone, Default)]
struct Recorded(Arc<Mutex<Vec<String>>>);

struct Values<'a>(&'a mut Vec<String>);

impl Visit for Values<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push(format!("{}={value:?}", field.name()));
    }
}

impl<S: Subscriber> Layer<S> for Recorded {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        event.record(&mut Values(&mut self.0.lock().unwrap()));
    }
}

impl Recorded {
    fn values(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

fn recording() -> (Recorded, tracing::subscriber::DefaultGuard) {
    let recorded = Recorded::default();
    let guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(recorded.clone()));
    (recorded, guard)
}

struct ClampLimit;

#[async_trait]
impl ToolGuardrail for ClampLimit {
    fn name(&self) -> &str {
        "clamp-limit"
    }

    async fn validate_call(&self, _tool: &str, args: &Value) -> ToolGuardrailResult {
        ToolGuardrailResult::revise(
            json!({"limit": 10}),
            format!("limit for {} lowered to 10", args["account"]),
        )
    }
}

#[tokio::test(flavor = "current_thread")]
async fn tool_guardrail_reasons_are_returned_but_not_logged() {
    let root = tempfile::tempdir().unwrap();
    let denied =
        ToolGuardrailSet::new().with(PathAllowList::new("notes-only", ["path"], [root.path()]));
    let revised = ToolGuardrailSet::new().with(ClampLimit);
    let (recorded, _guard) = recording();

    let decision =
        denied.evaluate("write_note", &json!({"path": format!("/tmp/{SECRET}/note.txt")})).await;
    let ToolCallDecision::Deny { reason, .. } = decision else { panic!("the call is denied") };
    assert!(reason.contains(SECRET), "the caller still gets the reason: {reason}");
    let decision = revised.evaluate("lookup", &json!({"account": SECRET, "limit": 500})).await;
    assert!(decision.is_allowed());

    let values = recorded.values();
    assert!(values.iter().all(|value| !value.contains(SECRET)), "{values:#?}");
    assert!(values.contains(&"guardrail=\"notes-only\"".to_string()), "{values:#?}");
    assert!(values.contains(&"guardrail=\"clamp-limit\"".to_string()), "{values:#?}");
}

struct RedactAccount {
    parallel: bool,
}

#[async_trait]
impl Guardrail for RedactAccount {
    fn name(&self) -> &str {
        "redact-account"
    }

    async fn validate(&self, _content: &Content) -> GuardrailResult {
        GuardrailResult::Transform {
            new_content: Content::new("model").with_text("[redacted]"),
            reason: format!("replaced {SECRET}"),
        }
    }

    fn run_parallel(&self) -> bool {
        self.parallel
    }
}

#[tokio::test(flavor = "current_thread")]
async fn transform_reasons_are_not_logged() {
    let guardrails = GuardrailSet::new()
        .with(RedactAccount { parallel: true })
        .with(RedactAccount { parallel: false });
    let (recorded, _guard) = recording();

    let result = GuardrailExecutor::run(&guardrails, &Content::new("model").with_text(SECRET))
        .await
        .unwrap();
    assert!(result.transformed_content.is_some());

    let values = recorded.values();
    assert!(values.iter().all(|value| !value.contains(SECRET)), "{values:#?}");
    assert_eq!(
        values.iter().filter(|value| *value == "guardrail=\"redact-account\"").count(),
        2,
        "both the parallel and the sequential transform are logged: {values:#?}"
    );
}

/// Revises a result on `lookup` and withholds one on `export`, quoting the result each time.
struct ScreensAccounts;

#[async_trait]
impl ToolGuardrail for ScreensAccounts {
    fn name(&self) -> &str {
        "screens-accounts"
    }

    async fn validate_result(
        &self,
        tool: &str,
        _args: &Value,
        result: &Value,
    ) -> ToolResultGuardrailResult {
        let reason = format!("the result names {}", result["account"]);
        if tool == "export" {
            ToolResultGuardrailResult::withhold(reason, Severity::High)
        } else {
            ToolResultGuardrailResult::revise(json!({"account": "[redacted]"}), reason)
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn tool_result_guardrail_reasons_are_returned_but_not_logged() {
    let guardrails = ToolGuardrailSet::new().with(ScreensAccounts);
    let (recorded, _guard) = recording();

    let revised =
        guardrails.evaluate_result("lookup", &json!({}), &json!({"account": SECRET})).await;
    assert_eq!(revised, ToolResultDecision::Deliver { result: json!({"account": "[redacted]"}) });
    let withheld =
        guardrails.evaluate_result("export", &json!({}), &json!({"account": SECRET})).await;
    let ToolResultDecision::Withhold { reason, .. } = withheld else {
        panic!("the result is withheld")
    };
    assert!(reason.contains(SECRET), "the caller still gets the reason: {reason}");

    let values = recorded.values();
    assert!(values.iter().all(|value| !value.contains(SECRET)), "{values:#?}");
    assert_eq!(
        values.iter().filter(|value| *value == "guardrail=\"screens-accounts\"").count(),
        2,
        "the revision and the withholding are both logged: {values:#?}"
    );
}
