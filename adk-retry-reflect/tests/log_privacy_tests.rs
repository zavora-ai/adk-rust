//! The plugin's log lines name a tool error by its category and code, and carry its text only
//! when payloads are recorded.
//!
//! A tool error can quote its arguments or a credential. The plugin logged the error text on
//! every retry, exhaustion, and skipped refusal, whatever the run's `record_payloads`.

use std::sync::{Arc, Mutex};

use adk_core::{
    AdkError, CallbackContext, Content, ErrorCategory, ErrorComponent, ReadonlyContext, Result,
    Tool, ToolContext, ToolEffect, ToolOutcome, async_trait,
};
use adk_plugin::{EnhancedPlugin, PluginContext};
use adk_retry_reflect::RetryReflectPluginBuilder;
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

struct Lookup(ToolEffect);

#[async_trait]
impl Tool for Lookup {
    fn name(&self) -> &str {
        "lookup_account"
    }
    fn description(&self) -> &str {
        "Looks up an account."
    }
    fn effect(&self) -> ToolEffect {
        self.0
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Ok(Value::Null)
    }
}

struct Ctx(Content, ToolOutcome);

#[async_trait]
impl ReadonlyContext for Ctx {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "agent"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn app_name(&self) -> &str {
        "app"
    }
    fn session_id(&self) -> &str {
        "session"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.0
    }
}

#[async_trait]
impl CallbackContext for Ctx {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
    fn tool_outcome(&self) -> Option<ToolOutcome> {
        Some(self.1.clone())
    }
}

/// Every log field the plugin writes for three failures of one call: two retries and the
/// exhaustion after them, then a timed-out non-idempotent call it skips.
async fn logged_fields(record_payloads: bool) -> Vec<String> {
    let plugin = RetryReflectPluginBuilder::new()
        .max_retries(2)
        .record_payloads(record_payloads)
        .build()
        .unwrap();
    let backend_error = || {
        AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Unavailable,
            "tool.backend_unavailable",
            format!("account service refused {SECRET}"),
        )
    };
    let failure = |effect: ToolEffect, error: AdkError| {
        let outcome = ToolOutcome {
            tool_name: "lookup_account".to_string(),
            tool_args: json!({"account": SECRET}),
            success: false,
            duration: std::time::Duration::from_millis(5),
            error_message: Some(error.message.clone()),
            error: Some(Arc::new(error)),
            attempt: 0,
        };
        (Arc::new(Lookup(effect)), Arc::new(Ctx(Content::new("user"), outcome)))
    };
    let recorded = Recorded::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(recorded.clone()));
    let response = json!({"error": format!("account service refused {SECRET}")});
    for _ in 0..3 {
        let (tool, ctx) = failure(ToolEffect::ReadOnly, backend_error());
        plugin
            .after_tool_call(
                tool,
                &json!({"account": SECRET}),
                response.clone(),
                ctx,
                &PluginContext::new(),
            )
            .await
            .unwrap();
    }
    let timeout = AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Timeout,
        "tool.timeout",
        format!("tool timed out charging {SECRET}"),
    );
    let (tool, ctx) = failure(ToolEffect::NonIdempotent, timeout);
    plugin
        .after_tool_call(
            tool,
            &json!({"account": SECRET}),
            json!({"error": format!("tool timed out charging {SECRET}")}),
            ctx,
            &PluginContext::new(),
        )
        .await
        .unwrap();
    recorded.0.lock().unwrap().clone()
}

#[tokio::test(flavor = "current_thread")]
async fn error_text_stays_out_of_logs_by_default() {
    let fields = logged_fields(false).await;
    let messages: Vec<&String> =
        fields.iter().filter(|field| field.starts_with("message=")).collect();
    assert_eq!(messages.len(), 4, "{fields:#?}");
    assert!(fields.iter().all(|field| !field.contains(SECRET)), "{fields:#?}");
    let errors: Vec<&String> = fields.iter().filter(|field| field.starts_with("error=")).collect();
    assert_eq!(errors.len(), 4, "{fields:#?}");
    assert!(
        errors[..3].iter().all(|field| field.contains("tool.backend_unavailable")),
        "{errors:#?}"
    );
    assert!(errors[3].contains("tool.timeout"), "{errors:#?}");
}

#[tokio::test(flavor = "current_thread")]
async fn error_text_is_logged_when_payloads_are_recorded() {
    let fields = logged_fields(true).await;
    let texts: Vec<&String> =
        fields.iter().filter(|field| field.starts_with("error.message=")).collect();
    assert_eq!(texts.len(), 4, "{fields:#?}");
    assert!(texts.iter().all(|field| field.contains(SECRET)), "{texts:#?}");
    assert!(
        fields
            .iter()
            .filter(|field| field.starts_with("error="))
            .all(|field| !field.contains(SECRET)),
        "{fields:#?}"
    );
}
