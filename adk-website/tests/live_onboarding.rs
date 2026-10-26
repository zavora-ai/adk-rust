//! The onboarding interview on a live model (spec 3.5 §7.18 stages 1–4, §11 "Live", A-I18,
//! A-I19, A-I20): the Steward, built with the product's onboarding instruction and the
//! business template, interviews a scripted owner who paired with the setup code. Facts are
//! `Known` only in the owner's own words, and the live preview rebuilds from them.
//!
//! Run with `ADK_ENV_FILE` naming a `.env` that holds `ANTHROPIC_API_KEY`, or with the key in
//! the workspace's `.env`:
//!
//! ```text
//! cargo nextest run -p adk-website --test live_onboarding --run-ignored all
//! ```

mod common;

use std::sync::Arc;

use adk_core::Part;
use adk_harness::{FactFilter, FactStatus, FactStore, HarnessMode};
use adk_kernel::pairing::{InMemoryDeviceStore, Pairing};
use adk_website::kinds::CharterInputs;
use adk_website::onboarding::build::LivePreview;
use adk_website::onboarding::{SetupGate, StartingPoint, steward_instruction};
use adk_website::team::AgentRole;
use awp_types::SiteKind;
use common::*;

const OWNER_TURNS: [&str; 2] = [
    "Hi! Nothing online yet. We are Juniper Bakery in Nakuru. We bake rye and sourdough every \
     morning, and we deliver within Nakuru town only.",
    "We open at seven every weekday and at eight on Saturdays. We're closed on Sundays.",
];

#[tokio::test]
#[ignore = "live: calls claude-sonnet-5-5 with ANTHROPIC_API_KEY"]
async fn live_interview_records_known_facts_only_from_the_owners_words() {
    let env = std::env::var_os("ADK_ENV_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.env"));
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
    let mut site =
        Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let gate = SetupGate::new(Pairing::new(Arc::new(InMemoryDeviceStore::new())), OWNER);
    let issued = gate.issue().await.unwrap();
    let token = gate.claim(&issued.code, "Amina's phone").await.unwrap().token;
    adk_website::jobs::register_onboarding(&site.ladder, site.site.id).await.unwrap();
    let inputs = CharterInputs::new(
        site.site.id,
        "Juniper Bakery",
        OWNER,
        "Fresh bread for Nakuru every morning.",
        vec!["en".parse().unwrap()],
        "KES",
    );
    let draft = site.template.draft_charter(&site.team, &inputs);
    let instruction = format!(
        "{}\n\nThe owner has paired their phone. Discovery found nothing to read. You are in stage 4, \
         the interview: record every fact the owner states with interview_record, quoting the \
         owner's exact words, before you reply; then ask one short next question.",
        steward_instruction("Juniper Bakery", &StartingPoint::Nothing, Some(&site.template))
    );
    let prompter = ScriptedPrompter::new();
    let kit_model = model.clone();
    let (steward, _) = site.back_office("steward", &draft, Vec::new(), move |b| {
        b.model(kit_model)
            .instruction(instruction)
            .mode(HarnessMode::Ask)
            .prompter(prompter)
            .max_iterations(12)
    });
    let preview = LivePreview::new(
        site.runtime.clone(),
        site.facts.clone(),
        site.partitions.fact_scope(OWNER),
        "Juniper Bakery",
        AgentRole::Steward.principal(&site.site),
    );
    let owner = user(OWNER);
    let session = steward.open_session(&owner, None).await.unwrap();
    let mut known_seen = 0;
    for turn in OWNER_TURNS {
        gate.admit(Some(&token)).await.expect("the paired device speaks");
        let events = collect(steward.send(&owner, &session, turn).await.unwrap()).await;
        for event in &events {
            for part in event.llm_response.content.iter().flat_map(|c| c.parts.iter()) {
                match part {
                    Part::FunctionCall { name, args, .. } => println!("call {name} {args}"),
                    Part::FunctionResponse { function_response, .. } => {
                        println!("result {} {}", function_response.name, function_response.response)
                    }
                    Part::Text { text } => println!("steward: {text}"),
                    _ => {}
                }
            }
        }
        let snapshot = preview.refresh(&draft).await.unwrap();
        println!("preview: {} known facts, published {}", snapshot.facts, snapshot.published);
        assert!(snapshot.facts >= known_seen, "the preview never loses a confirmed fact");
        known_seen = snapshot.facts;
    }
    let facts = site.facts.list(FactFilter::user(&site.site.name, OWNER)).await.unwrap();
    let known: Vec<_> = facts.iter().filter(|f| f.status == FactStatus::Known).collect();
    println!("{} facts, {} known", facts.len(), known.len());
    assert!(!known.is_empty(), "the model recorded no confirmed fact: {facts:#?}");
    let said = OWNER_TURNS.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
    for fact in &known {
        let quote = fact.provenance.quote.as_deref().expect("a Known fact keeps its quote");
        let quote = quote.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(said.contains(&quote), "a Known fact quotes words the owner never said: {fact:#?}");
    }
    assert_eq!(known_seen, known.len(), "the preview shows every Known fact");
}
