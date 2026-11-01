//! An agent binds its final answer's markdown links to the sources its run retrieved, and
//! `CitationRequired` checks the answer against them.
#![cfg(feature = "guardrails")]

mod test_context;

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_agent::guardrails::{
    CitationRequired, GuardrailSet, OUTPUT_BLOCKED_CODE, WhenNothingRetrieved,
};
use adk_core::{
    Agent, CitationMetadata, CitationSource, Content, ErrorCategory, Event, FinishReason,
    GroundingSource, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result, Tool,
    ToolContext, record_grounding_sources,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use test_context::TestContext;

const POLICY: &str = "https://example.com/policies/refunds";

/// Calls `search` first when `searches`, then answers with `answer`.
struct AnsweringModel {
    searches: bool,
    answer: &'static str,
}

#[async_trait]
impl Llm for AnsweringModel {
    fn name(&self) -> &str {
        "answering-model"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> Result<LlmResponseStream> {
        let searched = request.contents.iter().any(|content| {
            content.parts.iter().any(|part| matches!(part, Part::FunctionResponse { .. }))
        });
        let part = if self.searches && !searched {
            Part::FunctionCall {
                name: "search".to_string(),
                args: json!({ "query": "refunds" }),
                id: Some("call-1".to_string()),
                thought_signature: None,
            }
        } else {
            Part::Text { text: self.answer.to_string() }
        };
        let response = LlmResponse {
            content: Some(Content { role: "model".to_string(), parts: vec![part] }),
            turn_complete: true,
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// Returns one passage and records its source.
struct Search;

#[async_trait]
impl Tool for Search {
    fn name(&self) -> &str {
        "search"
    }

    fn description(&self) -> &str {
        "Searches the policies."
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        record_grounding_sources(
            ctx.as_ref(),
            [GroundingSource::new("search", POLICY).with_title("Refund policy")],
        );
        Ok(json!([{ "text": "Refunds take 30 days.", "source_uri": POLICY }]))
    }
}

async fn run(
    searches: bool,
    answer: &'static str,
    guardrails: Option<GuardrailSet>,
) -> Vec<Result<Event>> {
    let mut builder = LlmAgentBuilder::new("support")
        .model(Arc::new(AnsweringModel { searches, answer }))
        .tool(Arc::new(Search));
    if let Some(guardrails) = guardrails {
        builder = builder.output_guardrails(guardrails);
    }
    let agent = builder.build().expect("build agent");
    agent
        .run(Arc::new(TestContext::new("How long do refunds take?")))
        .await
        .expect("run starts")
        .collect()
        .await
}

fn final_citations(events: &[Result<Event>]) -> Option<CitationMetadata> {
    let last = events.last().expect("an event").as_ref().expect("no error");
    last.llm_response.citation_metadata.clone()
}

#[tokio::test]
async fn a_link_to_a_retrieved_source_becomes_a_citation_and_passes() {
    let answer = "Refunds take 30 days ([refund policy](https://example.com/policies/refunds)).";
    let events = run(true, answer, Some(GuardrailSet::new().with(CitationRequired::new()))).await;

    let tool_event = events
        .iter()
        .filter_map(|event| event.as_ref().ok())
        .find(|event| !event.actions.grounding_sources.is_empty())
        .expect("the tool's event carries its sources");
    assert_eq!(
        tool_event.actions.grounding_sources,
        [GroundingSource::new("search", POLICY).with_title("Refund policy")]
    );

    let start = "Refunds take 30 days (".chars().count() as i32;
    let end =
        start + "[refund policy](https://example.com/policies/refunds)".chars().count() as i32;
    assert_eq!(
        final_citations(&events),
        Some(CitationMetadata {
            citation_sources: vec![CitationSource {
                uri: Some(POLICY.to_string()),
                title: Some("Refund policy".to_string()),
                start_index: Some(start),
                end_index: Some(end),
                license: None,
                publication_date: None,
            }],
        })
    );
}

#[tokio::test]
async fn a_link_to_a_source_the_run_did_not_retrieve_is_no_citation() {
    let answer = "Refunds take 30 days ([policy](https://elsewhere.example/refunds)).";

    let events = run(true, answer, None).await;
    assert_eq!(final_citations(&events), None, "an unretrieved URI is not bound");

    let events = run(true, answer, Some(GuardrailSet::new().with(CitationRequired::new()))).await;
    let error = events.last().expect("an event").as_ref().expect_err("the answer is blocked");
    assert_eq!((error.category, error.code), (ErrorCategory::InvalidInput, OUTPUT_BLOCKED_CODE));
}

#[tokio::test]
async fn an_answer_without_retrieval_fails_unless_configured_to_pass() {
    let answer = "Hello! How can I help?";

    let events = run(false, answer, Some(GuardrailSet::new().with(CitationRequired::new()))).await;
    let error = events.last().expect("an event").as_ref().expect_err("nothing was retrieved");
    assert_eq!(error.code, OUTPUT_BLOCKED_CODE);

    let lenient = CitationRequired::new().when_nothing_retrieved(WhenNothingRetrieved::Pass);
    let events = run(false, answer, Some(GuardrailSet::new().with(lenient))).await;
    assert!(events.last().expect("an event").is_ok(), "{events:?}");
}
