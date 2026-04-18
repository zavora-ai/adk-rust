//! User simulator that generates persona-driven messages.

use std::sync::Arc;

use adk_core::model::{Llm, LlmRequest};
use adk_core::types::Content;
use futures::StreamExt;

use super::profile::PersonaProfile;
use crate::error::{EvalError, Result};

/// Drives multi-turn evaluation conversations by generating user messages
/// according to a [`PersonaProfile`].
///
/// The simulator uses a configurable LLM to produce messages that reflect
/// the persona's behavioral traits, pursue the persona's goals, and respect
/// the persona's constraints.
///
/// # Example
///
/// ```rust,ignore
/// use adk_eval::personas::{UserSimulator, PersonaProfile, PersonaTraits, Verbosity, ExpertiseLevel};
/// use std::sync::Arc;
///
/// let persona = PersonaProfile {
///     name: "impatient-dev".to_string(),
///     description: "A senior developer who wants quick answers".to_string(),
///     traits: PersonaTraits {
///         communication_style: "direct and terse".to_string(),
///         verbosity: Verbosity::Terse,
///         expertise_level: ExpertiseLevel::Expert,
///     },
///     goals: vec!["Get a working code example".to_string()],
///     constraints: vec!["Never ask for basic explanations".to_string()],
/// };
///
/// let simulator = UserSimulator::new(llm, persona);
/// let message = simulator.generate_message(&[]).await?;
/// ```
pub struct UserSimulator {
    /// The LLM used to generate persona-driven messages.
    llm: Arc<dyn Llm>,
    /// The persona definition driving message generation.
    persona: PersonaProfile,
}

impl UserSimulator {
    /// Create a new simulator with the given LLM and persona.
    pub fn new(llm: Arc<dyn Llm>, persona: PersonaProfile) -> Self {
        Self { llm, persona }
    }

    /// Returns a reference to the persona profile.
    pub fn persona(&self) -> &PersonaProfile {
        &self.persona
    }

    /// Generate the next user message given conversation history.
    ///
    /// Builds a system prompt from the persona definition and sends
    /// the conversation history to the configured LLM to produce a
    /// message that reflects the persona's traits, goals, and constraints.
    pub async fn generate_message(&self, history: &[Content]) -> Result<Content> {
        let system_prompt = self.build_system_prompt();

        let mut contents = Vec::with_capacity(history.len() + 1);

        // Add the system instruction as the first message
        contents.push(Content::new("user").with_text(&system_prompt));
        contents
            .push(Content::new("model").with_text("Understood. I will role-play as this persona."));

        // Add conversation history
        contents.extend_from_slice(history);

        // If history is empty, prompt the LLM to start the conversation
        if history.is_empty() {
            contents.push(
                Content::new("user")
                    .with_text("Start the conversation as this persona. Send your first message."),
            );
        } else {
            contents.push(
                Content::new("user").with_text(
                    "Continue the conversation as this persona. Send your next message.",
                ),
            );
        }

        let request = LlmRequest::new(self.llm.name(), contents);

        let mut stream = self
            .llm
            .generate_content(request, false)
            .await
            .map_err(|e| EvalError::ExecutionError(format!("LLM generation failed: {e}")))?;

        // Collect the full response from the stream
        let mut response_text = String::new();
        while let Some(result) = stream.next().await {
            let response =
                result.map_err(|e| EvalError::ExecutionError(format!("LLM stream error: {e}")))?;
            if let Some(content) = &response.content {
                for part in &content.parts {
                    if let Some(text) = part.text() {
                        response_text.push_str(text);
                    }
                }
            }
        }

        if response_text.is_empty() {
            return Err(EvalError::ExecutionError(
                "LLM returned empty response for persona message".to_string(),
            ));
        }

        Ok(Content::new("user").with_text(response_text))
    }

    /// Build a system prompt that instructs the LLM to role-play as the persona.
    fn build_system_prompt(&self) -> String {
        let persona = &self.persona;
        let traits = &persona.traits;

        let mut prompt = format!(
            "You are role-playing as a user persona named \"{}\".\n\
             Description: {}\n\n\
             Communication style: {}\n\
             Verbosity: {:?}\n\
             Expertise level: {:?}\n",
            persona.name,
            persona.description,
            traits.communication_style,
            traits.verbosity,
            traits.expertise_level,
        );

        if !persona.goals.is_empty() {
            prompt.push_str("\nGoals (pursue these during the conversation):\n");
            for goal in &persona.goals {
                prompt.push_str(&format!("- {goal}\n"));
            }
        }

        if !persona.constraints.is_empty() {
            prompt.push_str("\nConstraints (always respect these):\n");
            for constraint in &persona.constraints {
                prompt.push_str(&format!("- {constraint}\n"));
            }
        }

        prompt.push_str(
            "\nRespond ONLY with the persona's message. \
             Do not include any meta-commentary, labels, or prefixes. \
             Stay in character at all times.",
        );

        prompt
    }
}
