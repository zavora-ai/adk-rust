//! `decide` returns one option of a `Choice` type with the model's rationale and confidence,
//! asks for it with a response schema and no tools, and meters the call.

use std::sync::{Arc, Mutex};

use adk_core::{
    BudgetTracker, Choice, ChoiceOption, Content, Decider, ErrorCategory, Llm, LlmRequest,
    LlmResponse, LlmResponseStream, ModelCallScope, Result, RunBudget, decide,
};
use async_trait::async_trait;
use serde_json::json;

/// Answers every call with `reply` and keeps the requests it saw.
struct Replying {
    reply: String,
    seen: Mutex<Vec<LlmRequest>>,
}

impl Replying {
    fn new(reply: impl Into<String>) -> Arc<Self> {
        Arc::new(Self { reply: reply.into(), seen: Mutex::new(Vec::new()) })
    }

    fn requests(&self) -> Vec<LlmRequest> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl Llm for Replying {
    fn name(&self) -> &str {
        "claude-sonnet-5-5"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> Result<LlmResponseStream> {
        self.seen.lock().unwrap().push(request);
        let response = LlmResponse {
            content: Some(Content::new("model").with_text(self.reply.clone())),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter(vec![Ok(response)])))
    }
}

#[derive(Debug, PartialEq)]
enum Route {
    Billing,
    Technical,
}

impl Choice for Route {
    fn options() -> Vec<ChoiceOption> {
        vec![
            ChoiceOption::new("billing", "Invoices, refunds and payment problems"),
            ChoiceOption::new("technical", "Errors, outages and how-to questions"),
        ]
    }

    fn from_option(name: &str) -> Option<Self> {
        match name {
            "billing" => Some(Self::Billing),
            "technical" => Some(Self::Technical),
            _ => None,
        }
    }
}

#[tokio::test]
async fn a_decision_is_typed_and_asked_for_with_a_schema_and_no_tools() {
    let model = Replying::new(
        json!({"choice": "billing", "rationale": "The customer was charged twice.", "confidence": 0.93})
            .to_string(),
    );
    let decision = decide::<Route>(model.clone(), "I was charged twice this month").await.unwrap();
    assert_eq!(decision.choice, Route::Billing);
    assert_eq!(
        (decision.option.as_str(), decision.rationale.as_str(), decision.confidence),
        ("billing", "The customer was charged twice.", 0.93)
    );

    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.tools.is_empty(), "a decision declares no tool, so no tool is forced");
    let schema = request.config.as_ref().and_then(|config| config.response_schema.clone()).unwrap();
    assert_eq!(schema["properties"]["choice"]["enum"], json!(["billing", "technical"]));
    assert_eq!(schema["required"], json!(["choice", "rationale", "confidence"]));
    let prompt = serde_json::to_string(&request.contents).unwrap();
    assert!(prompt.contains("`technical`: Errors, outages and how-to questions"), "{prompt}");
    assert!(prompt.contains("I was charged twice this month"), "{prompt}");
}

#[tokio::test]
async fn a_reply_outside_the_options_is_an_error_not_a_guess() {
    let model = Replying::new(r#"{"choice": "sales", "rationale": "", "confidence": 0.99}"#);
    let error = decide::<Route>(model, "upgrade my plan").await.unwrap_err();
    assert_eq!(error.code, "decision.invalid_choice");
}

#[tokio::test]
async fn the_call_counts_against_the_scope_and_a_refused_call_never_reaches_the_model() {
    let model =
        Replying::new(r#"{"choice": "technical", "rationale": "an error", "confidence": 0.8}"#);
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(1)));
    let decider = Decider::new(model.clone())
        .with_instruction("Route the ticket.")
        .with_scope(ModelCallScope::new().with_budget(tracker.clone()));

    let first = decider.decide::<Route>("the app crashes").await.unwrap();
    assert_eq!(first.choice, Route::Technical);
    let prompt = serde_json::to_string(&model.requests()[0].contents).unwrap();
    assert!(prompt.contains("Route the ticket."), "{prompt}");

    let refused = decider.decide::<Route>("the app crashes again").await.unwrap_err();
    assert_eq!(refused.category, ErrorCategory::ResourceExhausted, "{refused}");
    assert_eq!(model.requests().len(), 1, "the refused call was not sent");
}

#[tokio::test]
async fn options_known_at_run_time_are_decided_among() {
    let model = Replying::new("urgent");
    let options = [
        ChoiceOption::new("urgent", "Production is down"),
        ChoiceOption::new("normal", "Everything else"),
    ];
    let decision = Decider::new(model).decide_among(&options, "prod is down").await.unwrap();
    assert_eq!((decision.choice.as_str(), decision.confidence), ("urgent", 0.0));
}
