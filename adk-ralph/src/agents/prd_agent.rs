//! PRD Agent for generating structured requirements from user prompts.
//!
//! The PRD Agent is the first phase in the Ralph pipeline:
//! 1. **PRD Agent** → generates requirements (prd.md)
//! 2. Architect Agent → generates design and tasks
//! 3. Ralph Loop Agent → implements tasks iteratively
//!
//! This agent uses LlmAgent with:
//! - `output_schema` to force structured JSON response
//! - `output_key` to store PRD in session state for downstream agents

use crate::models::ModelConfig;
use crate::{RalphError, Result};
use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Llm};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

/// Instruction prompt for the PRD Agent.
const PRD_INSTRUCTION: &str = r#"You are an expert Product Manager creating a Product Requirements Document (PRD).

Given a project description, generate a comprehensive PRD as structured JSON.

## CRITICAL: Be Comprehensive

Do NOT just implement what the user literally asked for. Think like a product manager:
- What features do similar products have? (e.g., for a calculator, look at macOS Calculator, Windows Calculator)
- What would users expect even if not explicitly mentioned?
- What edge cases need handling?
- What makes a product feel complete vs. half-baked?

For example, if asked for a "calculator", users expect:
- Basic operations (+, -, *, /)
- Scientific functions (sqrt, square, power, percentage, modulo)
- Decimal and negative number support
- Keyboard input support
- Error handling (division by zero, overflow, invalid input)
- Clear/reset functionality
- Exit command

## Guidelines

- Generate **8-15 user stories** for a complete product
- Use EARS patterns for acceptance criteria:
  - WHEN/THE for event-driven requirements
  - IF/THEN for conditional requirements  
  - WHILE for state-driven requirements
- Priority 1 = core MVP features, Priority 5 = future enhancements
- Be specific and testable in acceptance criteria
- **Include features users would expect** even if not explicitly requested
- Think about edge cases and error scenarios
"#;


/// PRD Agent that generates structured requirements using LlmAgent.
///
/// Uses the ADK agent framework with:
/// - Tools: write_file for saving PRD
/// - Output key: "prd" for session state
/// - Structured instruction for consistent format
pub struct PrdAgent {
    agent: Arc<dyn Agent + Send + Sync>,
    project_path: PathBuf,
}

impl std::fmt::Debug for PrdAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrdAgent")
            .field("name", &self.agent.name())
            .field("project_path", &self.project_path)
            .finish()
    }
}

impl PrdAgent {
    /// Create a new builder for PrdAgent.
    pub fn builder() -> PrdAgentBuilder {
        PrdAgentBuilder::default()
    }

    /// Get the instruction prompt.
    pub fn instruction() -> &'static str {
        PRD_INSTRUCTION
    }

    /// Get the underlying agent for running.
    pub fn agent(&self) -> Arc<dyn Agent + Send + Sync> {
        self.agent.clone()
    }

    /// Get the project path.
    pub fn project_path(&self) -> &PathBuf {
        &self.project_path
    }
}

/// Builder for creating a PrdAgent with fluent API.
pub struct PrdAgentBuilder {
    model: Option<Arc<dyn Llm>>,
    model_config: ModelConfig,
    output_path: PathBuf,
    project_path: PathBuf,
}

impl std::fmt::Debug for PrdAgentBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrdAgentBuilder")
            .field("model", &self.model.as_ref().map(|m| m.name()))
            .field("model_config", &self.model_config)
            .field("output_path", &self.output_path)
            .field("project_path", &self.project_path)
            .finish()
    }
}

impl Default for PrdAgentBuilder {
    fn default() -> Self {
        Self {
            model: None,
            model_config: ModelConfig::new("gemini", "gemini-2.5-pro-preview-05-06"),
            output_path: PathBuf::from("prd.md"),
            project_path: PathBuf::from("."),
        }
    }
}

impl PrdAgentBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn model(mut self, model: Arc<dyn Llm>) -> Self {
        self.model = Some(model);
        self
    }

    pub fn model_config(mut self, config: ModelConfig) -> Self {
        self.model_config = config;
        self
    }

    pub fn output_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.output_path = path.into();
        self
    }

    pub fn project_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.project_path = path.into();
        self
    }

    pub async fn build(self) -> Result<PrdAgent> {
        let model = match self.model {
            Some(m) => m,
            None => create_model_from_config(&self.model_config).await?,
        };

        // Define the JSON schema for structured PRD output
        let prd_schema = json!({
            "type": "object",
            "properties": {
                "project_name": {
                    "type": "string",
                    "description": "Name of the project"
                },
                "overview": {
                    "type": "string",
                    "description": "Detailed description of the project scope, goals, and target users"
                },
                "user_stories": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "User story ID (e.g., US-001)"
                            },
                            "title": {
                                "type": "string",
                                "description": "Short title for the user story"
                            },
                            "priority": {
                                "type": "integer",
                                "description": "Priority 1-5 (1=critical, 5=nice-to-have)"
                            },
                            "story": {
                                "type": "string",
                                "description": "As a [role], I want [feature], so that [benefit]"
                            },
                            "acceptance_criteria": {
                                "type": "array",
                                "items": {
                                    "type": "string"
                                },
                                "description": "EARS-pattern acceptance criteria"
                            }
                        },
                        "required": ["id", "title", "priority", "story", "acceptance_criteria"]
                    },
                    "description": "List of user stories (8-15 recommended)"
                },
                "technical_constraints": {
                    "type": "array",
                    "items": {
                        "type": "string"
                    },
                    "description": "Technical requirements or limitations"
                },
                "success_metrics": {
                    "type": "array",
                    "items": {
                        "type": "string"
                    },
                    "description": "How success will be measured"
                }
            },
            "required": ["project_name", "overview", "user_stories"]
        });

        // Build the LlmAgent with output_schema for structured response
        let agent = LlmAgentBuilder::new("prd-agent")
            .description("Generates Product Requirements Document from project description")
            .model(model)
            .instruction(PRD_INSTRUCTION)
            .output_schema(prd_schema)
            .output_key("prd") // Store output in session state
            .build()
            .map_err(|e| RalphError::Agent {
                agent: "prd".to_string(),
                message: e.to_string(),
            })?;

        Ok(PrdAgent {
            agent: Arc::new(agent),
            project_path: self.project_path,
        })
    }
}


/// Create an LLM model from configuration.
async fn create_model_from_config(config: &ModelConfig) -> Result<Arc<dyn Llm>> {
    use std::env;

    let model: Arc<dyn Llm> = match config.provider.to_lowercase().as_str() {
        "anthropic" => {
            use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

            let api_key = env::var("ANTHROPIC_API_KEY").map_err(|_| {
                RalphError::Configuration("ANTHROPIC_API_KEY environment variable not set".into())
            })?;
            let anthropic_config = AnthropicConfig::new(api_key, &config.model_name);
            let client = AnthropicClient::new(anthropic_config).map_err(|e| RalphError::Model {
                provider: "anthropic".into(),
                message: e.to_string(),
            })?;
            Arc::new(client)
        }
        "openai" => {
            use adk_model::openai::{OpenAIClient, OpenAIConfig};

            let api_key = env::var("OPENAI_API_KEY").map_err(|_| {
                RalphError::Configuration("OPENAI_API_KEY environment variable not set".into())
            })?;
            let openai_config = OpenAIConfig::new(api_key, &config.model_name);
            let client = OpenAIClient::new(openai_config).map_err(|e| RalphError::Model {
                provider: "openai".into(),
                message: e.to_string(),
            })?;
            Arc::new(client)
        }
        "gemini" => {
            use adk_model::gemini::GeminiModel;

            let api_key = env::var("GEMINI_API_KEY")
                .or_else(|_| env::var("GOOGLE_API_KEY"))
                .map_err(|_| {
                    RalphError::Configuration(
                        "GEMINI_API_KEY or GOOGLE_API_KEY environment variable not set".into(),
                    )
                })?;
            let client = GeminiModel::new(api_key, &config.model_name).map_err(|e| {
                RalphError::Model {
                    provider: "gemini".into(),
                    message: e.to_string(),
                }
            })?;
            Arc::new(client)
        }
        provider => {
            return Err(RalphError::Configuration(format!(
                "Unsupported model provider: {}. Supported: anthropic, openai, gemini",
                provider
            )));
        }
    };

    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prd_agent_builder_defaults() {
        let builder = PrdAgentBuilder::default();
        assert!(builder.model.is_none());
        assert_eq!(builder.model_config.provider, "gemini");
        assert_eq!(builder.output_path, PathBuf::from("prd.md"));
        assert_eq!(builder.project_path, PathBuf::from("."));
    }

    #[test]
    fn test_prd_agent_builder_fluent_api() {
        let config = ModelConfig::new("openai", "gpt-4o");
        let builder = PrdAgentBuilder::new()
            .model_config(config)
            .output_path("custom_prd.md")
            .project_path("/tmp/project");

        assert_eq!(builder.model_config.provider, "openai");
        assert_eq!(builder.output_path, PathBuf::from("custom_prd.md"));
        assert_eq!(builder.project_path, PathBuf::from("/tmp/project"));
    }

    #[test]
    fn test_prd_instruction_content() {
        let instruction = PrdAgent::instruction();
        assert!(instruction.contains("PRD"));
        assert!(instruction.contains("user stories"));
        assert!(instruction.contains("EARS"));
    }
}


impl PrdAgent {
    /// Generate a PRD from a user prompt by running the agent.
    ///
    /// This method:
    /// 1. Creates a session for the agent
    /// 2. Runs the agent with the prompt (returns structured JSON)
    /// 3. Writes the PRD to prd.md
    /// 4. Returns the parsed PRD document
    pub async fn generate(&self, prompt: &str) -> Result<crate::models::PrdDocument> {
        use adk_core::{Content, Part};
        use adk_runner::{Runner, RunnerConfig};
        use adk_session::{CreateRequest, InMemorySessionService, SessionService};
        use futures::StreamExt;

        // Create session service
        let session_service: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());

        // Create a session first
        let session_id = format!("prd-{}", uuid::Uuid::new_v4());
        session_service
            .create(CreateRequest {
                app_name: "ralph-prd".to_string(),
                user_id: "user".to_string(),
                session_id: Some(session_id.clone()),
                state: std::collections::HashMap::new(),
            })
            .await
            .map_err(|e| RalphError::Agent {
                agent: "prd".to_string(),
                message: format!("Failed to create session: {}", e),
            })?;

        // Create runner
        let runner = Runner::new(RunnerConfig {
            app_name: "ralph-prd".to_string(),
            agent: self.agent.clone(),
            session_service,
            artifact_service: None,
            memory_service: None,
            run_config: None,
        }).map_err(|e| RalphError::Agent {
            agent: "prd".to_string(),
            message: e.to_string(),
        })?;

        // Create user content with the prompt
        let user_content = Content {
            role: "user".to_string(),
            parts: vec![Part::Text {
                text: format!("Generate a PRD for the following project:\n\n{}", prompt),
            }],
        };

        // Run the agent and collect the response
        let mut stream = runner
            .run("user".to_string(), session_id, user_content)
            .await
            .map_err(|e| RalphError::Agent {
                agent: "prd".to_string(),
                message: e.to_string(),
            })?;

        // Collect all text from the response
        let mut response_text = String::new();
        while let Some(result) = stream.next().await {
            match result {
                Ok(event) => {
                    if let Some(content) = &event.llm_response.content {
                        for part in &content.parts {
                            if let Part::Text { text } = part {
                                response_text.push_str(text);
                            }
                        }
                    }
                }
                Err(e) => {
                    return Err(RalphError::Agent {
                        agent: "prd".to_string(),
                        message: e.to_string(),
                    });
                }
            }
        }

        // Parse the JSON response
        let prd_json: serde_json::Value = serde_json::from_str(&response_text)
            .map_err(|e| RalphError::Prd(format!("Failed to parse PRD JSON: {} - Response: {}", e, &response_text[..response_text.len().min(500)])))?;

        // Convert JSON to PrdDocument
        let prd = json_to_prd_document(&prd_json)?;

        // Write the PRD as markdown
        let prd_path = self.project_path.join("prd.md");
        let markdown = prd_to_markdown(&prd);
        std::fs::write(&prd_path, &markdown)
            .map_err(|e| RalphError::Prd(format!("Failed to write PRD file: {}", e)))?;

        Ok(prd)
    }
}

/// Convert JSON response to PrdDocument
fn json_to_prd_document(json: &serde_json::Value) -> Result<crate::models::PrdDocument> {
    use crate::models::{AcceptanceCriterion, PrdDocument, UserStory};

    let project = json["project_name"]
        .as_str()
        .unwrap_or("Untitled Project")
        .to_string();

    let overview = json["overview"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let user_stories: Vec<UserStory> = json["user_stories"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|story| {
                    let mut us = UserStory::new(
                        story["id"].as_str().unwrap_or("US-000"),
                        story["title"].as_str().unwrap_or(""),
                        story["story"].as_str().unwrap_or(""),
                        story["priority"].as_i64().unwrap_or(3) as u32,
                    );
                    
                    // Add acceptance criteria
                    if let Some(criteria) = story["acceptance_criteria"].as_array() {
                        for (i, crit) in criteria.iter().enumerate() {
                            if let Some(text) = crit.as_str() {
                                us.acceptance_criteria.push(AcceptanceCriterion::new(
                                    (i + 1).to_string(),
                                    text,
                                ));
                            }
                        }
                    }
                    
                    us
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(PrdDocument {
        project,
        overview,
        language: None,
        user_stories,
        version: "1.0".to_string(),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
        updated_at: None,
    })
}

/// Convert PrdDocument to markdown format
fn prd_to_markdown(prd: &crate::models::PrdDocument) -> String {
    // Use the built-in to_markdown method
    prd.to_markdown()
}
