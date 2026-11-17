//! Typed decisions: a model picks one of a fixed set of options and states why and how sure
//! it is.
//!
//! | Item | Use |
//! |---|---|
//! | [`Choice`] | a type whose values a model can decide, such as an enum of routes |
//! | [`decide`] | one decision of type `T` with a model, in the current [`ModelCallScope`] |
//! | [`Decider`] | the same with an instruction, an explicit scope or an output cap; [`Decider::decide_among`] decides among options known only at run time |
//! | [`Decision`] | the choice, the option's name, the model's rationale and its confidence |
//!
//! The request carries a JSON response schema whose `choice` is an `enum` of the option names,
//! so a provider with structured outputs (Anthropic `output_config.format`, OpenAI `strict`
//! JSON schema, Gemini `responseSchema`) constrains the reply to a valid option. No tool is
//! declared and no `tool_choice` is forced, which models that reject forced tool use accept.
//! A provider without structured outputs is asked for the same JSON in the prompt, and the
//! reply is validated: a choice that names no option is an error, never a guess.
//!
//! Every call goes through a [`ModelCallScope`], so it counts against the run's budget and
//! spend ledger and is refused before it reaches the provider when a limit is reached.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_core::{Choice, ChoiceOption, Llm, decide};
//!
//! #[derive(Debug, PartialEq)]
//! enum Route {
//!     Billing,
//!     Technical,
//! }
//!
//! impl Choice for Route {
//!     fn options() -> Vec<ChoiceOption> {
//!         vec![
//!             ChoiceOption::new("billing", "Invoices, refunds and payment problems"),
//!             ChoiceOption::new("technical", "Errors, outages and how-to questions"),
//!         ]
//!     }
//!
//!     fn from_option(name: &str) -> Option<Self> {
//!         match name {
//!             "billing" => Some(Self::Billing),
//!             "technical" => Some(Self::Technical),
//!             _ => None,
//!         }
//!     }
//! }
//!
//! async fn route(model: Arc<dyn Llm>) -> adk_core::Result<()> {
//!     let decision = decide::<Route>(model, "I was charged twice this month").await?;
//!     println!("{:?} ({:.2}): {}", decision.choice, decision.confidence, decision.rationale);
//!     Ok(())
//! }
//! ```

use std::collections::HashSet;
use std::sync::Arc;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    AdkError, Content, ErrorCategory, ErrorComponent, GenerateContentConfig, Llm, LlmRequest,
    ModelCallScope, Part, Result,
};

/// One option of a decision: the name the model answers with and what it means.
///
/// # Example
///
/// ```rust
/// let option = adk_core::ChoiceOption::new("billing", "Invoices, refunds and payment problems");
/// assert_eq!(option.name, "billing");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ChoiceOption {
    /// The name the model answers with.
    pub name: String,
    /// What choosing it means, shown to the model.
    pub description: String,
}

impl ChoiceOption {
    /// An option named `name`, described by `description`.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self { name: name.into(), description: description.into() }
    }
}

/// A type whose values a model can decide among: a fixed set of named options.
///
/// Implement it for an enum of routes, labels or next actions, or derive it with
/// `#[derive(Choice)]` from `adk-rust-macros`, then call [`decide`]. Option names must be
/// distinct and non-empty; [`from_option`](Self::from_option) maps each name back to its value.
///
/// # Example
///
/// ```rust
/// use adk_core::{Choice, ChoiceOption};
///
/// enum Next {
///     Answer,
///     Escalate,
/// }
///
/// impl Choice for Next {
///     fn options() -> Vec<ChoiceOption> {
///         vec![
///             ChoiceOption::new("answer", "The question can be answered from the docs"),
///             ChoiceOption::new("escalate", "A person must decide"),
///         ]
///     }
///
///     fn from_option(name: &str) -> Option<Self> {
///         match name {
///             "answer" => Some(Self::Answer),
///             "escalate" => Some(Self::Escalate),
///             _ => None,
///         }
///     }
/// }
///
/// assert!(matches!(Next::from_option("escalate"), Some(Next::Escalate)));
/// ```
pub trait Choice: Sized {
    /// The options, in the order the model sees them.
    fn options() -> Vec<ChoiceOption>;

    /// The value of the option named `name`, or `None` when no option has that name.
    fn from_option(name: &str) -> Option<Self>;
}

/// What a model decided: the choice, the name of the option it picked, why, and how sure it
/// is.
///
/// # Example
///
/// ```rust
/// let decision: adk_core::Decision<String> = serde_json::from_value(serde_json::json!({
///     "choice": "billing", "option": "billing", "rationale": "a double charge", "confidence": 0.9
/// }))?;
/// assert_eq!(decision.option, "billing");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Decision<T> {
    /// The value decided.
    pub choice: T,
    /// The name of the option picked.
    pub option: String,
    /// The model's reason, in its words; empty when it gave none.
    pub rationale: String,
    /// The model's confidence from 0.0 to 1.0; 0.0 when it stated none.
    pub confidence: f64,
}

impl<T> Decision<T> {
    /// The same decision with its choice mapped by `f`.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Decision<U> {
        Decision {
            choice: f(self.choice),
            option: self.option,
            rationale: self.rationale,
            confidence: self.confidence,
        }
    }
}

/// Decides one [`Choice`] with `model` about `input`, in the current [`ModelCallScope`].
///
/// # Errors
///
/// See [`Decider::decide_among`].
///
/// # Example
///
/// See the [module documentation](self).
pub async fn decide<T: Choice>(model: Arc<dyn Llm>, input: &str) -> Result<Decision<T>> {
    Decider::new(model).decide(input).await
}

/// Makes typed decisions with a model: an instruction, an optional explicit
/// [`ModelCallScope`], and an output cap.
///
/// Without [`with_scope`](Self::with_scope) a call is metered against the scope current
/// when it runs ([`ModelCallScope::within`]), as `LlmAgent` tools and workflow agents set
/// it, and runs unmetered outside one.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_core::{ChoiceOption, Decider, Llm};
///
/// async fn triage(model: Arc<dyn Llm>, ticket: &str) -> adk_core::Result<String> {
///     let options = [
///         ChoiceOption::new("urgent", "Production is down or data is at risk"),
///         ChoiceOption::new("normal", "Everything else"),
///     ];
///     let decision = Decider::new(model)
///         .with_instruction("Triage the support ticket.")
///         .decide_among(&options, ticket)
///         .await?;
///     Ok(decision.choice)
/// }
/// ```
#[derive(Clone)]
pub struct Decider {
    model: Arc<dyn Llm>,
    instruction: Option<String>,
    scope: Option<ModelCallScope>,
    max_output_tokens: Option<i32>,
}

impl std::fmt::Debug for Decider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decider")
            .field("model", &self.model.name())
            .field("instruction", &self.instruction)
            .field("scoped", &self.scope.is_some())
            .finish_non_exhaustive()
    }
}

/// The decision code of a reply that names no option.
const INVALID_CHOICE: &str = "decision.invalid_choice";

impl Decider {
    /// A decider that asks `model`.
    pub fn new(model: Arc<dyn Llm>) -> Self {
        Self { model, instruction: None, scope: None, max_output_tokens: None }
    }

    /// What to decide and how, shown to the model before the options; by default "Decide
    /// which option fits the input best."
    #[must_use]
    pub fn with_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// Meters every call against `scope`, whether or not another scope is current.
    #[must_use]
    pub fn with_scope(mut self, scope: ModelCallScope) -> Self {
        self.scope = Some(scope);
        self
    }

    /// Caps the reply; by default the provider's limit applies, which leaves room for models
    /// that count thinking against it.
    #[must_use]
    pub fn with_max_output_tokens(mut self, tokens: i32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    /// Decides one value of `T` about `input`.
    ///
    /// # Errors
    ///
    /// See [`decide_among`](Self::decide_among); also `decision.invalid_options` when
    /// [`Choice::from_option`] does not map one of `T`'s own option names.
    pub async fn decide<T: Choice>(&self, input: &str) -> Result<Decision<T>> {
        let decision = self.decide_among(&T::options(), input).await?;
        let Some(choice) = T::from_option(&decision.option) else {
            return Err(invalid_options(format!(
                "option `{}` is one of the type's options but from_option does not map it",
                decision.option
            )));
        };
        Ok(decision.map(|_| choice))
    }

    /// Decides one of `options` about `input`; the decision's choice is the option's name.
    ///
    /// # Errors
    ///
    /// Returns `decision.invalid_options` when `options` is empty or repeats or leaves out a
    /// name, `decision.invalid_choice` when the reply names no option, and the model's error
    /// when the call fails or is refused, such as an exhausted run budget.
    pub async fn decide_among(
        &self,
        options: &[ChoiceOption],
        input: &str,
    ) -> Result<Decision<String>> {
        validate(options)?;
        let mut listed = String::new();
        for option in options {
            listed.push_str(&format!("- `{}`: {}\n", option.name, option.description));
        }
        let instruction =
            self.instruction.as_deref().unwrap_or("Decide which option fits the input best.");
        let prompt = format!(
            "{instruction}\n\nOptions:\n{listed}\n<input>\n{input}\n</input>\n\nReply with only a \
             JSON object: {{\"choice\": \"<the name of one option>\", \"rationale\": \"<why, \
             in one or two sentences>\", \"confidence\": <a number from 0 to 1>}}."
        );
        let config = GenerateContentConfig {
            response_schema: Some(decision_schema(options)),
            max_output_tokens: self.max_output_tokens,
            ..Default::default()
        };
        let request =
            LlmRequest::new(self.model.name(), vec![Content::new("user").with_text(prompt)])
                .with_config(config);
        let mut stream = match &self.scope {
            Some(scope) => scope.generate(self.model.as_ref(), request, false).await?,
            None => crate::generate_metered(self.model.as_ref(), request, false).await?,
        };
        let mut reply = String::new();
        while let Some(response) = stream.next().await {
            let response = response?;
            if response.partial {
                continue;
            }
            if let Some(content) = &response.content {
                reply.extend(content.parts.iter().filter_map(Part::text));
            }
        }
        let decision = read_decision(&reply, options)?;
        tracing::debug!(
            decision.option = %decision.option,
            decision.confidence = decision.confidence,
            "decision made"
        );
        Ok(decision)
    }
}

fn invalid_options(message: String) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::InvalidInput,
        "decision.invalid_options",
        message,
    )
}

fn validate(options: &[ChoiceOption]) -> Result<()> {
    if options.is_empty() {
        return Err(invalid_options("a decision needs at least one option".to_string()));
    }
    let mut seen = HashSet::new();
    for option in options {
        if option.name.trim().is_empty() {
            return Err(invalid_options("an option of a decision has an empty name".to_string()));
        }
        if !seen.insert(option.name.as_str()) {
            return Err(invalid_options(format!(
                "the options of a decision repeat the name `{}`; give each a distinct name",
                option.name
            )));
        }
    }
    Ok(())
}

/// The response schema of a decision, in the subset every provider's structured output
/// accepts: no numeric bounds, every property required, closed objects.
fn decision_schema(options: &[ChoiceOption]) -> Value {
    let names: Vec<&str> = options.iter().map(|option| option.name.as_str()).collect();
    json!({
        "type": "object",
        "properties": {
            "choice": {"type": "string", "enum": names},
            "rationale": {"type": "string"},
            "confidence": {"type": "number"}
        },
        "required": ["choice", "rationale", "confidence"],
        "additionalProperties": false
    })
}

/// The option `answer` names: exactly, else ignoring case and the quotes, backticks and
/// trailing period a model may wrap a name in.
fn match_option<'a>(answer: &str, options: &'a [ChoiceOption]) -> Option<&'a ChoiceOption> {
    let cleaned = answer.trim().trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | '.')).trim();
    options
        .iter()
        .find(|option| option.name == cleaned)
        .or_else(|| options.iter().find(|option| option.name.eq_ignore_ascii_case(cleaned)))
}

/// The first JSON object in `text`: the whole text, a fenced block, or the first balanced
/// braces in prose.
fn json_object(text: &str) -> Option<serde_json::Map<String, Value>> {
    let trimmed = text.trim();
    if let Ok(Value::Object(object)) = serde_json::from_str(trimmed) {
        return Some(object);
    }
    let start = trimmed.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in trimmed[start..].char_indices() {
        match ch {
            _ if escaped => escaped = false,
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    let candidate = &trimmed[start..start + offset + 1];
                    return match serde_json::from_str(candidate) {
                        Ok(Value::Object(object)) => Some(object),
                        _ => None,
                    };
                }
            }
            _ => {}
        }
    }
    None
}

/// Reads the decision in `reply`: its JSON object, or a reply that is only an option's name.
fn read_decision(reply: &str, options: &[ChoiceOption]) -> Result<Decision<String>> {
    let names = || options.iter().map(|option| option.name.as_str()).collect::<Vec<_>>().join(", ");
    let (answer, rationale, confidence) = match json_object(reply) {
        Some(object) => (
            object.get("choice").and_then(Value::as_str).unwrap_or_default().to_string(),
            object.get("rationale").and_then(Value::as_str).unwrap_or_default().trim().to_string(),
            object
                .get("confidence")
                .and_then(Value::as_f64)
                .filter(|confidence| confidence.is_finite())
                .map_or(0.0, |confidence| confidence.clamp(0.0, 1.0)),
        ),
        // A provider that ignores the schema may answer with the bare name.
        None => (reply.to_string(), String::new(), 0.0),
    };
    let option = match_option(&answer, options).ok_or_else(|| {
        AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Internal,
            INVALID_CHOICE,
            format!(
                "the model's decision names no option (expected one of: {}); its reply: {}",
                names(),
                truncate(reply, 300)
            ),
        )
    })?;
    Ok(Decision { choice: option.name.clone(), option: option.name.clone(), rationale, confidence })
}

fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Vec<ChoiceOption> {
        vec![
            ChoiceOption::new("billing", "Invoices and refunds"),
            ChoiceOption::new("technical", "Errors and outages"),
        ]
    }

    #[test]
    fn a_json_reply_is_read_and_its_confidence_clamped() {
        let decision = read_decision(
            "```json\n{\"choice\": \"Billing\", \"rationale\": \"double charge\", \"confidence\": 1.4}\n```",
            &options(),
        )
        .unwrap();
        assert_eq!(
            decision,
            Decision {
                choice: "billing".to_string(),
                option: "billing".to_string(),
                rationale: "double charge".to_string(),
                confidence: 1.0,
            }
        );
    }

    #[test]
    fn a_bare_option_name_is_a_decision_without_rationale() {
        let decision = read_decision(" `technical`. ", &options()).unwrap();
        assert_eq!((decision.option.as_str(), decision.confidence), ("technical", 0.0));
    }

    #[test]
    fn a_reply_that_names_no_option_is_refused() {
        for reply in [
            "{\"choice\": \"sales\", \"rationale\": \"\", \"confidence\": 0.9}",
            "probably billing?",
        ] {
            let error = read_decision(reply, &options()).unwrap_err();
            assert_eq!(error.code, INVALID_CHOICE, "{reply}");
            assert!(error.message.contains("billing, technical"), "{error}");
        }
    }

    #[test]
    fn options_must_be_distinct_and_named() {
        assert!(validate(&options()).is_ok());
        assert!(validate(&[]).is_err());
        let repeated = [ChoiceOption::new("a", "x"), ChoiceOption::new("a", "y")];
        assert_eq!(validate(&repeated).unwrap_err().code, "decision.invalid_options");
        assert!(validate(&[ChoiceOption::new(" ", "x")]).is_err());
    }

    #[test]
    fn the_schema_enumerates_the_options_without_bounds() {
        let schema = decision_schema(&options());
        assert_eq!(schema["properties"]["choice"]["enum"], json!(["billing", "technical"]));
        assert_eq!(schema["additionalProperties"], json!(false));
        let text = schema.to_string();
        assert!(!text.contains("minimum") && !text.contains("maximum"), "{text}");
    }
}
