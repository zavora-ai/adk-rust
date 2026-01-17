//! Architect Agent for creating system design and task breakdown.
//!
//! The Architect Agent reads the PRD document and generates:
//! - `design.md` - System architecture with component diagrams
//! - `tasks.json` - Structured task list with priorities and dependencies
//!
//! This agent uses LlmAgent with:
//! - `read_file` tool to read PRD
//! - `write_file` tool to save design and tasks
//! - `output_key` to store outputs in session state
//! - Session state access to read PRD from previous agent

use crate::models::ModelConfig;
use crate::{RalphError, Result};
use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Llm};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

/// Instruction prompt for the Architect Agent.
const ARCHITECT_INSTRUCTION: &str = r#"You are an expert software architect.

Your task is to analyze the PRD provided below and create a system design with task breakdown.

## Generate Structured Output

Generate a JSON response with:

### Design Section
- project: Project name from PRD
- overview: High-level architecture description
- language: Target programming language (detect from PRD or default to Rust)
- technology_stack: Testing framework, build tool, key dependencies
- architecture_diagram: Mermaid flowchart showing components and data flow
- components: Array of components with name, purpose, file path, key functions, dependencies
- file_structure: Project structure specification with directories and files arrays
- design_decisions: Key architectural decisions with rationale

### Tasks Section
- tasks: Array of implementation tasks

## Project Structure Specification

Create a complete project structure specification in file_structure:
- directories: Array of directories to create (if any), relative to project root
- files: Array of files to create, relative to project root

CRITICAL RULES:
1. The project root directory already exists - do NOT include it in paths
2. Use paths relative to project root (e.g., "main.go" not "./main.go")
3. Do NOT create wrapper directories named after the project
4. Follow standard conventions for the target language
5. Keep structure minimal - only create necessary directories

### Examples by Language

Go CLI:
```json
{
  "directories": [],
  "files": ["main.go", "go.mod"]
}
```

Rust binary:
```json
{
  "directories": ["src"],
  "files": ["Cargo.toml", "src/main.rs"]
}
```

Python script:
```json
{
  "directories": [],
  "files": ["main.py", "requirements.txt"]
}
```

TypeScript/Node.js:
```json
{
  "directories": ["src"],
  "files": ["package.json", "tsconfig.json", "src/index.ts"]
}
```

## Guidelines for Tasks

- Create required tasks to meet acceptance criteria
- Priority: 1=critical (do first), 5=nice-to-have
- Complexity: "low", "medium", or "high"
- First task should set up project structure (no dependencies)
- Each task should be completable in one coding session
- Link tasks to user stories from PRD
- Order logically: setup → core features → enhancements → tests
- In files_to_create and files_to_modify, use paths relative to project root
"#;


/// Architect Agent that creates system design and task breakdown using LlmAgent.
///
/// Uses the ADK agent framework with:
/// - Tools: read_file, write_file
/// - Output key: "design" for session state
/// - Reads PRD from session state or file
pub struct ArchitectAgent {
    agent: Arc<dyn Agent + Send + Sync>,
    project_path: PathBuf,
}

impl std::fmt::Debug for ArchitectAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchitectAgent")
            .field("name", &self.agent.name())
            .field("project_path", &self.project_path)
            .finish()
    }
}

impl ArchitectAgent {
    /// Create a new builder for ArchitectAgent.
    pub fn builder() -> ArchitectAgentBuilder {
        ArchitectAgentBuilder::default()
    }

    /// Get the instruction prompt.
    pub fn instruction() -> &'static str {
        ARCHITECT_INSTRUCTION
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

/// Builder for creating an ArchitectAgent with fluent API.
pub struct ArchitectAgentBuilder {
    model: Option<Arc<dyn Llm>>,
    model_config: ModelConfig,
    prd_path: PathBuf,
    design_path: PathBuf,
    tasks_path: PathBuf,
    project_path: PathBuf,
}

impl std::fmt::Debug for ArchitectAgentBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchitectAgentBuilder")
            .field("model", &self.model.as_ref().map(|m| m.name()))
            .field("model_config", &self.model_config)
            .field("prd_path", &self.prd_path)
            .field("design_path", &self.design_path)
            .field("tasks_path", &self.tasks_path)
            .field("project_path", &self.project_path)
            .finish()
    }
}

impl Default for ArchitectAgentBuilder {
    fn default() -> Self {
        Self {
            model: None,
            model_config: ModelConfig::new("gemini", "gemini-2.5-pro-preview-05-06"),
            prd_path: PathBuf::from("prd.md"),
            design_path: PathBuf::from("design.md"),
            tasks_path: PathBuf::from("tasks.json"),
            project_path: PathBuf::from("."),
        }
    }
}

impl ArchitectAgentBuilder {
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

    pub fn prd_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.prd_path = path.into();
        self
    }

    pub fn design_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.design_path = path.into();
        self
    }

    pub fn tasks_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.tasks_path = path.into();
        self
    }

    pub fn project_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.project_path = path.into();
        self
    }

    pub async fn build(self) -> Result<ArchitectAgent> {
        let model = match self.model {
            Some(m) => m,
            None => create_model_from_config(&self.model_config).await?,
        };

        // Define the JSON schema for structured design + tasks output
        let architect_schema = json!({
            "type": "object",
            "properties": {
                "design": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Project name"
                        },
                        "overview": {
                            "type": "string",
                            "description": "High-level architecture description"
                        },
                        "language": {
                            "type": "string",
                            "description": "Target programming language"
                        },
                        "technology_stack": {
                            "type": "object",
                            "properties": {
                                "testing": { "type": "string" },
                                "build_tool": { "type": "string" },
                                "key_dependencies": {
                                    "type": "array",
                                    "items": { "type": "string" }
                                }
                            },
                            "required": ["testing", "build_tool"]
                        },
                        "architecture_diagram": {
                            "type": "string",
                            "description": "Mermaid flowchart diagram"
                        },
                        "components": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": { "type": "string" },
                                    "purpose": { "type": "string" },
                                    "file": { "type": "string" },
                                    "key_functions": {
                                        "type": "array",
                                        "items": { "type": "string" }
                                    },
                                    "dependencies": {
                                        "type": "array",
                                        "items": { "type": "string" }
                                    }
                                },
                                "required": ["name", "purpose", "file"]
                            }
                        },
                        "file_structure": {
                            "type": "object",
                            "description": "Project structure specification with directories and files to create",
                            "properties": {
                                "directories": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Directories to create (relative to project root, e.g., 'src', 'tests')"
                                },
                                "files": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Files to create (relative to project root, e.g., 'main.go', 'src/lib.rs')"
                                }
                            },
                            "required": ["files"]
                        },
                        "design_decisions": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "decision": { "type": "string" },
                                    "rationale": { "type": "string" }
                                },
                                "required": ["decision", "rationale"]
                            }
                        }
                    },
                    "required": ["project", "overview", "language", "components"]
                },
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Task ID (e.g., TASK-001)"
                            },
                            "title": {
                                "type": "string",
                                "description": "Short title"
                            },
                            "description": {
                                "type": "string",
                                "description": "Detailed description"
                            },
                            "priority": {
                                "type": "integer",
                                "description": "Priority 1-5 (1=critical)"
                            },
                            "user_story_id": {
                                "type": "string",
                                "description": "Related user story ID"
                            },
                            "estimated_complexity": {
                                "type": "string",
                                "enum": ["low", "medium", "high"]
                            },
                            "dependencies": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Task IDs this depends on"
                            },
                            "files_to_create": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "files_to_modify": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "acceptance_criteria": {
                                "type": "array",
                                "items": { "type": "string" }
                            }
                        },
                        "required": ["id", "title", "description", "priority", "estimated_complexity"]
                    }
                }
            },
            "required": ["design", "tasks"]
        });

        // Build the LlmAgent with output_schema for structured response (no tools)
        let agent = LlmAgentBuilder::new("architect-agent")
            .description("Creates system design and task breakdown from PRD")
            .model(model)
            .instruction(ARCHITECT_INSTRUCTION)
            .output_schema(architect_schema)
            .output_key("architect_output") // Store output in session state
            .build()
            .map_err(|e| RalphError::Agent {
                agent: "architect".to_string(),
                message: e.to_string(),
            })?;

        Ok(ArchitectAgent {
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
            let client = GeminiModel::new(api_key, &config.model_name).map_err(|e| RalphError::Model {
                provider: "gemini".into(),
                message: e.to_string(),
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
    fn test_architect_agent_builder_defaults() {
        let builder = ArchitectAgentBuilder::default();
        assert!(builder.model.is_none());
        assert_eq!(builder.model_config.provider, "gemini");
        assert_eq!(builder.prd_path, PathBuf::from("prd.md"));
        assert_eq!(builder.design_path, PathBuf::from("design.md"));
        assert_eq!(builder.tasks_path, PathBuf::from("tasks.json"));
    }

    #[test]
    fn test_architect_instruction_content() {
        let instruction = ArchitectAgent::instruction();
        assert!(instruction.contains("design"));
        assert!(instruction.contains("tasks"));
        assert!(instruction.contains("components"));
    }
}


impl ArchitectAgent {
    /// Generate design and tasks by running the agent.
    ///
    /// This method:
    /// 1. Reads the PRD file
    /// 2. Creates a session for the agent
    /// 3. Runs the agent with PRD content (returns structured JSON)
    /// 4. Parses the JSON and writes design.md + tasks.json
    /// 5. Returns the parsed documents
    pub async fn generate(&self) -> Result<(crate::models::DesignDocument, crate::models::TaskList)> {
        use adk_core::{Content, Part};
        use adk_runner::{Runner, RunnerConfig};
        use adk_session::{CreateRequest, InMemorySessionService, SessionService};
        use futures::StreamExt;

        // Read the PRD file first
        let prd_path = self.project_path.join("prd.md");
        let prd_content = std::fs::read_to_string(&prd_path)
            .map_err(|e| RalphError::Prd(format!("Failed to read PRD file: {}", e)))?;

        // Create session service
        let session_service: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());

        // Create a session first
        let session_id = format!("architect-{}", uuid::Uuid::new_v4());
        session_service
            .create(CreateRequest {
                app_name: "ralph-architect".to_string(),
                user_id: "user".to_string(),
                session_id: Some(session_id.clone()),
                state: std::collections::HashMap::new(),
            })
            .await
            .map_err(|e| RalphError::Agent {
                agent: "architect".to_string(),
                message: format!("Failed to create session: {}", e),
            })?;

        // Create runner
        let runner = Runner::new(RunnerConfig {
            app_name: "ralph-architect".to_string(),
            agent: self.agent.clone(),
            session_service,
            artifact_service: None,
            memory_service: None,
            run_config: None,
        }).map_err(|e| RalphError::Agent {
            agent: "architect".to_string(),
            message: e.to_string(),
        })?;

        // Create user content with the PRD included
        let user_content = Content {
            role: "user".to_string(),
            parts: vec![Part::Text {
                text: format!(
                    "Generate the system design and task breakdown for the following PRD:\n\n---\n{}\n---",
                    prd_content
                ),
            }],
        };

        // Run the agent and collect the structured JSON response
        let mut stream = runner
            .run("user".to_string(), session_id, user_content)
            .await
            .map_err(|e| RalphError::Agent {
                agent: "architect".to_string(),
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
                        agent: "architect".to_string(),
                        message: e.to_string(),
                    });
                }
            }
        }

        // Parse the JSON response
        let architect_json: serde_json::Value = serde_json::from_str(&response_text)
            .map_err(|e| RalphError::Design(format!(
                "Failed to parse architect JSON: {} - Response: {}", 
                e, 
                &response_text[..response_text.len().min(500)]
            )))?;

        // Convert JSON to DesignDocument and TaskList
        let design = json_to_design_document(&architect_json["design"])?;
        let tasks = json_to_task_list(&architect_json, &design.project)?;

        // Write design.md
        let design_path = self.project_path.join("design.md");
        let design_markdown = design.to_markdown();
        std::fs::write(&design_path, &design_markdown)
            .map_err(|e| RalphError::Design(format!("Failed to write design.md: {}", e)))?;

        // Write tasks.json
        let tasks_path = self.project_path.join("tasks.json");
        tasks.save(&tasks_path).map_err(RalphError::Task)?;

        Ok((design, tasks))
    }
}

/// Convert JSON to DesignDocument
fn json_to_design_document(json: &serde_json::Value) -> Result<crate::models::DesignDocument> {
    use crate::models::{Component, DesignDocument, TechnologyStack};

    let project = json["project"]
        .as_str()
        .unwrap_or("Untitled Project")
        .to_string();

    let overview = json["overview"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let language = json["language"]
        .as_str()
        .unwrap_or("rust")
        .to_string();

    let technology_stack = TechnologyStack {
        language: language.clone(),
        testing_framework: json["technology_stack"]["testing"]
            .as_str()
            .unwrap_or("cargo test")
            .to_string(),
        build_tool: json["technology_stack"]["build_tool"]
            .as_str()
            .unwrap_or("cargo")
            .to_string(),
        dependencies: json["technology_stack"]["key_dependencies"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        additional: std::collections::HashMap::new(),
    };

    let component_diagram = json["architecture_diagram"]
        .as_str()
        .map(String::from);

    let components: Vec<Component> = json["components"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|c| Component {
                    name: c["name"].as_str().unwrap_or("").to_string(),
                    purpose: c["purpose"].as_str().unwrap_or("").to_string(),
                    file_path: c["file"].as_str().map(String::from),
                    interface: c["key_functions"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                    dependencies: c["dependencies"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();

    let design_decisions: Vec<String> = json["design_decisions"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|d| {
                    let decision = d["decision"].as_str().unwrap_or("");
                    let rationale = d["rationale"].as_str().unwrap_or("");
                    format!("{}: {}", decision, rationale)
                })
                .collect()
        })
        .unwrap_or_default();

    // Parse file_structure - handle both new object format and legacy string format
    let file_structure = parse_file_structure(&json["file_structure"], &project);

    Ok(DesignDocument {
        project,
        overview,
        component_diagram,
        components,
        file_structure,
        technology_stack: Some(technology_stack),
        design_decisions,
        version: "1.0".to_string(),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
        updated_at: None,
    })
}

/// Parse file_structure from JSON - handles both new object format and legacy string format.
fn parse_file_structure(json: &serde_json::Value, project_name: &str) -> Option<crate::models::FileStructure> {
    use crate::models::FileStructure;

    // Handle new structured format: { "directories": [...], "files": [...] }
    if json.is_object() {
        let directories: Vec<String> = json["directories"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let files: Vec<String> = json["files"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        if files.is_empty() && directories.is_empty() {
            return None;
        }

        // Build a FileStructure tree from the flat lists
        let mut root = FileStructure::directory(project_name, "Project root");

        // Add directories
        for dir in &directories {
            // Strip any leading ./ or project name prefix
            let clean_path = clean_path(dir, project_name);
            if !clean_path.is_empty() {
                add_path_to_structure(&mut root, &clean_path, true);
            }
        }

        // Add files
        for file in &files {
            // Strip any leading ./ or project name prefix
            let clean_path = clean_path(file, project_name);
            if !clean_path.is_empty() {
                add_path_to_structure(&mut root, &clean_path, false);
            }
        }

        return Some(root);
    }

    // Handle legacy string format (best-effort parsing)
    if let Some(text) = json.as_str() {
        if text.is_empty() {
            return None;
        }

        tracing::warn!("Legacy string file_structure format detected - consider updating to structured format");

        // Simple parsing: treat each non-empty line as a file path
        let mut root = FileStructure::directory(project_name, "Project root");
        for line in text.lines() {
            let trimmed = line.trim().trim_start_matches("- ").trim_start_matches("* ");
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                let clean_path = clean_path(trimmed, project_name);
                if !clean_path.is_empty() {
                    let is_dir = clean_path.ends_with('/');
                    let path = clean_path.trim_end_matches('/');
                    add_path_to_structure(&mut root, path, is_dir);
                }
            }
        }

        return Some(root);
    }

    None
}

/// Clean a path by removing leading ./ and project name prefix.
fn clean_path(path: &str, project_name: &str) -> String {
    let mut clean = path.trim();

    // Remove leading ./
    if let Some(stripped) = clean.strip_prefix("./") {
        clean = stripped;
    }

    // Remove leading project name prefix (e.g., "hello-go/main.go" -> "main.go")
    let project_prefix = format!("{}/", project_name);
    if let Some(stripped) = clean.strip_prefix(&project_prefix) {
        tracing::warn!(
            "Stripped redundant project name prefix from path: {} -> {}",
            path,
            stripped
        );
        clean = stripped;
    }

    // Also check for common variations
    let project_lower = project_name.to_lowercase().replace(' ', "-");
    let project_prefix_lower = format!("{}/", project_lower);
    if let Some(stripped) = clean.strip_prefix(&project_prefix_lower) {
        tracing::warn!(
            "Stripped redundant project name prefix from path: {} -> {}",
            path,
            stripped
        );
        clean = stripped;
    }

    clean.to_string()
}

/// Add a path to the FileStructure tree, creating intermediate directories as needed.
fn add_path_to_structure(root: &mut crate::models::FileStructure, path: &str, is_directory: bool) {
    use crate::models::FileStructure;

    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return;
    }

    let mut current = root;

    for (i, part) in parts.iter().enumerate() {
        let is_last = i == parts.len() - 1;
        let should_be_dir = !is_last || is_directory;

        // Find or create the child
        let existing_idx = current.children.iter().position(|c| c.name == *part);

        if let Some(idx) = existing_idx {
            if !is_last {
                current = &mut current.children[idx];
            }
        } else {
            let new_node = if should_be_dir {
                FileStructure::directory(*part, "")
            } else {
                FileStructure::file(*part, "")
            };
            current.children.push(new_node);

            if !is_last {
                let last_idx = current.children.len() - 1;
                current = &mut current.children[last_idx];
            }
        }
    }
}

/// Convert JSON to TaskList
fn json_to_task_list(json: &serde_json::Value, project: &str) -> Result<crate::models::TaskList> {
    use crate::models::{Task, TaskComplexity, TaskList, TaskStatus};

    let tasks: Vec<Task> = json["tasks"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|t| {
                    let complexity = match t["estimated_complexity"].as_str().unwrap_or("medium") {
                        "low" => TaskComplexity::Low,
                        "high" => TaskComplexity::High,
                        _ => TaskComplexity::Medium,
                    };

                    Task {
                        id: t["id"].as_str().unwrap_or("TASK-000").to_string(),
                        title: t["title"].as_str().unwrap_or("").to_string(),
                        description: t["description"].as_str().unwrap_or("").to_string(),
                        priority: t["priority"].as_i64().unwrap_or(3) as u32,
                        status: TaskStatus::Pending,
                        dependencies: t["dependencies"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                        user_story_id: t["user_story_id"].as_str().map(String::from),
                        estimated_complexity: complexity,
                        files_created: t["files_to_create"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                        files_modified: t["files_to_modify"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                        commit_hash: None,
                        attempts: 0,
                        notes: t["acceptance_criteria"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join("\n"))
                            .unwrap_or_default(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let language = json["design"]["language"]
        .as_str()
        .unwrap_or("rust")
        .to_string();

    Ok(TaskList {
        project: project.to_string(),
        language,
        phases: Vec::new(),
        tasks,
        version: "1.0".to_string(),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
        updated_at: None,
    })
}
