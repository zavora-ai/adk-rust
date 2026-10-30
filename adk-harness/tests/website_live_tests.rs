//! The onboarding interview on a live model (spec 3.5 §7.18 stage 4, A-I18): the Steward,
//! built with `website_back_office`, records the owner's answers with `interview_record`,
//! and a fact is `Known` only when its quote is the owner's own words.
//!
//! Run with `ADK_ENV_FILE` naming a `.env` that holds `ANTHROPIC_API_KEY`, or with the key in
//! the workspace's `.env`:
//!
//! ```text
//! cargo nextest run -p adk-harness --features website --run-ignored all --test integration \
//!   -E 'test(/^website_live_tests::/)'
//! ```

#![cfg(feature = "website")]

use crate::common;
use crate::website_common;

use std::path::PathBuf;
use std::sync::Arc;

use adk_core::Part;
use adk_harness::website::OnboardingStage;
use adk_harness::{FactFilter, FactStatus, FactStore, HarnessMode};
use common::{collect, user};
use website_common::{OWNER, World};

const OWNER_ANSWER: &str = "We are Juniper Bakery in Nakuru. We open at seven every weekday \
and at eight on Saturdays, and we are closed on Sundays. Our best seller is the rye loaf with \
caraway. We deliver within Nakuru town only.";

#[tokio::test]
#[ignore = "live: calls claude-sonnet-5-5 with ANTHROPIC_API_KEY"]
async fn live_interview_records_known_facts_only_from_the_owners_words() {
    let env = std::env::var_os("ADK_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.env"));
    let _ = dotenvy::from_path_override(&env);
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
        println!("skipped: no ANTHROPIC_API_KEY");
        return;
    };
    let model: Arc<dyn adk_core::Llm> = Arc::new(
        adk_model::anthropic::AnthropicClient::new(adk_model::anthropic::AnthropicConfig::new(
            key,
            "claude-sonnet-5-5",
        ))
        .unwrap(),
    );
    let world = World::new(true, false).await;
    let kit = world.back_office_kit();
    let rules = world.rules.clone();
    let dir = tempfile::tempdir().unwrap();
    let step = OnboardingStage::Interview.step(Vec::<String>::new());
    let steward = adk_harness::HarnessAgent::builder("steward")
        .model(model)
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(common::no_user_file())
        .without_compaction()
        .website_back_office(&kit)
        .standing_rules(rules)
        .mode(HarnessMode::Ask)
        .instruction(format!(
            "You are the Steward of Juniper Bakery's website, onboarding its owner.\n\nCurrent \
             step — {}: {}\nRecord every fact the owner states with interview_record before \
             you reply; then thank the owner in one sentence.",
            step.title, step.instruction
        ))
        .max_iterations(12)
        .build()
        .unwrap()
        .into_harness(&world.site.name, world.sessions.clone())
        .unwrap();
    let owner = user(OWNER);
    let session = steward.open_session(&owner, None).await.unwrap();
    let events = collect(steward.send(&owner, &session, OWNER_ANSWER).await.unwrap()).await;
    for event in &events {
        for part in event.llm_response.content.iter().flat_map(|content| content.parts.iter()) {
            match part {
                Part::FunctionCall { name, args, .. } => println!("call {name} {args}"),
                Part::FunctionResponse { function_response, .. } => {
                    println!("result {} {}", function_response.name, function_response.response)
                }
                Part::Text { text } => println!("text {text}"),
                _ => {}
            }
        }
    }
    let facts = world.facts.list(FactFilter::user(&world.site.name, OWNER)).await.unwrap();
    let known: Vec<_> = facts.iter().filter(|fact| fact.status == FactStatus::Known).collect();
    println!("{} facts, {} known", facts.len(), known.len());
    assert!(!known.is_empty(), "the model recorded no confirmed fact: {facts:#?}");
    let said = OWNER_ANSWER.split_whitespace().collect::<Vec<_>>().join(" ");
    for fact in &known {
        let quote = fact.provenance.quote.as_deref().expect("a Known fact keeps its quote");
        let quote = quote.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(said.contains(&quote), "a Known fact quotes words the owner never said: {fact:#?}");
    }
}
