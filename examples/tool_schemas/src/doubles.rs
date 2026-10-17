//! Scenario `scripted_llm_records_requests`: the scripted `Llm` test double in `adk-model`.

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Llm, Part};
use adk_model::{ScriptedLlm, ScriptedResponse};
use serde_json::json;

use crate::tools::{BookSlots, bookings, reset_bookings};
use crate::{Verdict, run_agent, verdict};

/// A two-turn tool loop on `adk_model::ScriptedLlm`: each call gets its own scripted reply, and
/// every request the agent sends is recorded.
pub async fn scripted_llm_records_requests() -> anyhow::Result<Verdict> {
    reset_bookings();
    let llm = Arc::new(ScriptedLlm::new(
        "scripted-model",
        [
            ScriptedResponse::function_call(
                "book_slots",
                json!({"room": "C3", "count": 2, "confirm_to": {"street": "9 Hill Rd", "city": "Kisumu"}}),
            ),
            ScriptedResponse::text("Booked room C3."),
        ],
    ));
    let agent = LlmAgentBuilder::new("booker")
        .model(llm.clone() as Arc<dyn Llm>)
        .instruction("Book meeting rooms with book_slots.")
        .tool(Arc::new(BookSlots))
        .build()?;
    let outcome = run_agent(agent, "Book two slots in C3, confirmation to Kisumu.").await?;

    let requests = llm.requests();
    let second_has_result = requests.get(1).is_some_and(|request| {
        request.contents.iter().flat_map(|content| content.parts.iter()).any(|part| {
            matches!(part, Part::FunctionResponse { function_response, .. }
                if function_response.name == "book_slots"
                    && function_response.response["booked"] == json!(true))
        })
    });
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (requests.len() == 2, format!("{} requests recorded (expected 2)", requests.len())),
        (
            requests.first().is_some_and(|request| request.tools.contains_key("book_slots")),
            "the first recorded request declares book_slots".to_string(),
        ),
        (second_has_result, "the second recorded request carries the book_slots result".into()),
        (bookings().len() == 1, format!("book_slots ran {} time(s)", bookings().len())),
        (
            outcome.final_text() == "Booked room C3.",
            format!("final reply: {:?}", outcome.final_text()),
        ),
    ]))
}
