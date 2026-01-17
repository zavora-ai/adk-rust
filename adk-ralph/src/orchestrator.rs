//! Ralph Orchestrator for coordinating the multi-agent pipeline.
//!
//! The orchestrator manages the three-phase development pipeline:
//! 1. PRD Agent → generates requirements (prd.md)
//! 2. Architect Agent → generates design and tasks (design.md, tasks.json)
//! 3. Ralph Loop Agent → implements tasks iteratively
//!
//! ## Requirements Validated
//!
//! - 1.6: WHEN the PRD is complete, THE PRD_Agent SHALL signal readiness for architecture phase
//! - 2.1: WHEN the PRD is approved, THE Architect_Agent SHALL read the `prd.md` file

use crate::agents::{ArchitectAgent, CompletionStatus, PrdAgent, RalphLoopAgent};
use crate::models::{DesignDocument, PrdDocument, RalphConfig, TaskList};
use crate::output::RalphOutput;
use crate::telemetry::{
    architect_design_span, log_completion, log_error, prd_generation_span, start_timing,
};
use crate::{RalphError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{info, instrument, warn};

/// Phase of the Ralph pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PipelinePhase {
    /// Requirements generation phase (PRD Agent)
    Requirements,
    /// Design and task breakdown phase (Architect Agent)
    Design,
    /// Implementation phase (Ralph Loop Agent)
    Implementation,
    /// Pipeline complete
    Complete,
}

impl std::fmt::Display for PipelinePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelinePhase::Requirements => write!(f, "Requirements"),
            PipelinePhase::Design => write!(f, "Design"),
            PipelinePhase::Implementation => write!(f, "Implementation"),
            PipelinePhase::Complete => write!(f, "Complete"),
        }
    }
}

/// State of the orchestrator.
#[derive(Debug, Clone)]
pub struct OrchestratorState {
    /// Current phase
    pub phase: PipelinePhase,
    /// PRD document (populated after requirements phase)
    pub prd: Option<PrdDocument>,
    /// Design document (populated after design phase)
    pub design: Option<DesignDocument>,
    /// Task list (populated after design phase)
    pub tasks: Option<TaskList>,
    /// Final completion status (populated after implementation phase)
    pub completion_status: Option<CompletionStatus>,
}

impl Default for OrchestratorState {
    fn default() -> Self {
        Self {
            phase: PipelinePhase::Requirements,
            prd: None,
            design: None,
            tasks: None,
            completion_status: None,
        }
    }
}

/// Ralph Orchestrator that coordinates the multi-agent pipeline.
///
/// The orchestrator sequences the three agents:
/// 1. PRD Agent → Architect Agent → Ralph Loop Agent
///
/// It handles phase transitions and manages overall state.
///
/// # Example
///
/// ```rust,ignore
/// use adk_ralph::{RalphConfig, RalphOrchestrator};
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let config = RalphConfig::from_env()?;
///     let orchestrator = RalphOrchestrator::new(config)?;
///     
///     let status = orchestrator.run("Create a CLI calculator in Rust").await?;
///     println!("Completed: {}", status);
///     Ok(())
/// }
/// ```
pub struct RalphOrchestrator {
    /// Configuration
    config: RalphConfig,
    /// Project base directory
    project_path: PathBuf,
    /// Current state
    state: OrchestratorState,
    /// Output handler for human-readable progress
    output: RalphOutput,
}

impl std::fmt::Debug for RalphOrchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RalphOrchestrator")
            .field("config", &self.config)
            .field("project_path", &self.project_path)
            .field("phase", &self.state.phase)
            .finish()
    }
}

impl RalphOrchestrator {
    /// Create a new orchestrator with the given configuration.
    pub fn new(config: RalphConfig) -> Result<Self> {
        config.validate()?;

        let project_path = PathBuf::from(&config.project_path);
        let output = RalphOutput::new(config.debug_level);

        // Create project directory if it doesn't exist
        if !project_path.exists() {
            std::fs::create_dir_all(&project_path).map_err(|e| {
                RalphError::Configuration(format!(
                    "Failed to create project directory '{}': {}",
                    project_path.display(),
                    e
                ))
            })?;
            info!(path = %project_path.display(), "Created project directory");
        }

        Ok(Self {
            config,
            project_path,
            state: OrchestratorState::default(),
            output,
        })
    }

    /// Create a new orchestrator builder.
    pub fn builder() -> OrchestratorBuilder {
        OrchestratorBuilder::default()
    }

    /// Get the current configuration.
    pub fn config(&self) -> &RalphConfig {
        &self.config
    }

    /// Get the current phase.
    pub fn phase(&self) -> PipelinePhase {
        self.state.phase
    }

    /// Get the current state.
    pub fn state(&self) -> &OrchestratorState {
        &self.state
    }

    /// Get the project path.
    pub fn project_path(&self) -> &PathBuf {
        &self.project_path
    }

    /// Check if a PRD file already exists.
    pub fn prd_exists(&self) -> bool {
        self.project_path.join(&self.config.prd_path).exists()
    }

    /// Check if a design file already exists.
    pub fn design_exists(&self) -> bool {
        self.project_path.join(&self.config.design_path).exists()
    }

    /// Check if a tasks file already exists.
    pub fn tasks_exist(&self) -> bool {
        self.project_path.join(&self.config.tasks_path).exists()
    }


    /// Run the requirements phase (PRD generation).
    ///
    /// This phase:
    /// 1. Takes the user prompt
    /// 2. Generates a PRD document with user stories
    /// 3. Saves to prd.md
    ///
    /// Uses the PRD Agent to generate structured requirements from the user prompt.
    #[instrument(skip(self, prompt), fields(phase = "requirements"))]
    pub async fn run_requirements_phase(&mut self, prompt: &str) -> Result<PrdDocument> {
        info!("Starting requirements phase");
        let _timing = start_timing("requirements_phase");

        // Create span for PRD generation
        let span = prd_generation_span(&self.config.agents.prd_model.model_name);
        let _guard = span.enter();

        // Check if PRD already exists
        let prd_path = self.project_path.join(&self.config.prd_path);
        if prd_path.exists() {
            self.output.status("Found existing PRD, loading...");
            info!("PRD file already exists, loading it");
            let prd = PrdDocument::load_markdown(&prd_path)
                .map_err(RalphError::Prd)?;
            self.state.prd = Some(prd.clone());
            self.state.phase = PipelinePhase::Design;
            return Ok(prd);
        }

        // Use PRD Agent to generate requirements
        self.output.status("Generating requirements with PRD Agent...");
        info!("Using PRD Agent to generate requirements");
        
        let prd_agent = PrdAgent::builder()
            .model_config(self.config.agents.prd_model.clone())
            .output_path(&self.config.prd_path)
            .project_path(&self.project_path)
            .build()
            .await?;

        let prd = prd_agent.generate(prompt).await?;

        self.output.status(&format!("Saved PRD to {}", self.config.prd_path));
        info!(
            prd_path = %prd_path.display(),
            user_stories = prd.user_stories.len(),
            "PRD generated and saved"
        );

        // Update state
        self.state.prd = Some(prd.clone());
        self.state.phase = PipelinePhase::Design;

        Ok(prd)
    }

    /// Run the design phase (Architect Agent).
    ///
    /// This phase:
    /// 1. Reads the PRD document
    /// 2. Generates system design (design.md)
    /// 3. Generates task breakdown (tasks.json)
    #[instrument(skip(self), fields(phase = "design"))]
    pub async fn run_design_phase(&mut self) -> Result<(DesignDocument, TaskList)> {
        info!("Starting design phase");
        let _timing = start_timing("design_phase");

        // Ensure we have a PRD
        let prd = self.state.prd.as_ref().ok_or_else(|| {
            RalphError::Prd("PRD not available. Run requirements phase first.".to_string())
        })?;

        // Detect language for span
        let language = prd.language.clone().unwrap_or_else(|| "rust".to_string());

        // Create span for architect design
        let span = architect_design_span(&self.config.agents.architect_model.model_name, &language);
        let _guard = span.enter();

        // Check if design and tasks already exist
        let design_path = self.project_path.join(&self.config.design_path);
        let tasks_path = self.project_path.join(&self.config.tasks_path);

        if design_path.exists() && tasks_path.exists() {
            self.output.status("Found existing design and tasks, loading...");
            info!("Design and tasks files already exist, loading them");
            let design = DesignDocument::load_markdown(&design_path)
                .map_err(RalphError::Design)?;
            let tasks = TaskList::load(&tasks_path)
                .map_err(RalphError::Task)?;
            
            self.state.design = Some(design.clone());
            self.state.tasks = Some(tasks.clone());
            self.state.phase = PipelinePhase::Implementation;
            
            return Ok((design, tasks));
        }

        // Create and run the Architect Agent
        self.output.status("Generating system design with Architect Agent...");
        let architect = ArchitectAgent::builder()
            .model_config(self.config.agents.architect_model.clone())
            .prd_path(&self.config.prd_path)
            .design_path(&self.config.design_path)
            .tasks_path(&self.config.tasks_path)
            .project_path(&self.project_path)
            .build()
            .await?;

        let (design, tasks) = architect.generate().await?;

        self.output.status(&format!(
            "Saved design to {}, tasks to {}",
            self.config.design_path, self.config.tasks_path
        ));
        info!(
            design_path = %design_path.display(),
            tasks_path = %tasks_path.display(),
            task_count = tasks.get_stats().total,
            "Design and tasks generated"
        );

        // Update state
        self.state.design = Some(design.clone());
        self.state.tasks = Some(tasks.clone());
        self.state.phase = PipelinePhase::Implementation;

        Ok((design, tasks))
    }


    /// Run the implementation phase (Ralph Loop Agent).
    ///
    /// This phase:
    /// 1. Reads tasks and design
    /// 2. Iteratively implements tasks
    /// 3. Returns completion status
    #[instrument(skip(self), fields(phase = "implementation"))]
    pub async fn run_implementation_phase(&mut self) -> Result<CompletionStatus> {
        info!("Starting implementation phase");
        let _timing = start_timing("implementation_phase");

        // Ensure we have design and tasks
        if self.state.design.is_none() || self.state.tasks.is_none() {
            return Err(RalphError::Design(
                "Design and tasks not available. Run design phase first.".to_string(),
            ));
        }

        // Create and run the Ralph Loop Agent
        let ralph_loop = RalphLoopAgent::builder()
            .config(self.config.clone())
            .project_path(&self.project_path)
            .build()
            .await?;

        let status = ralph_loop.run().await?;

        info!(status = %status, "Implementation phase complete");

        // Update state
        self.state.completion_status = Some(status.clone());
        self.state.phase = PipelinePhase::Complete;

        // Log completion event
        match &status {
            CompletionStatus::Complete {
                iterations,
                tasks_completed,
                message,
            } => {
                log_completion(*tasks_completed, *iterations, message);
            }
            CompletionStatus::MaxIterationsReached {
                iterations,
                tasks_completed,
                tasks_remaining,
            } => {
                warn!(
                    iterations = iterations,
                    tasks_completed = tasks_completed,
                    tasks_remaining = tasks_remaining,
                    "Max iterations reached"
                );
            }
            CompletionStatus::AllTasksBlocked {
                iterations,
                tasks_completed,
                tasks_blocked,
                reason,
            } => {
                log_error(
                    "implementation",
                    &format!(
                        "All tasks blocked after {} iterations: {} completed, {} blocked ({})",
                        iterations, tasks_completed, tasks_blocked, reason
                    ),
                );
            }
        }

        Ok(status)
    }

    /// Run the full pipeline from prompt to completion.
    ///
    /// This is the main entry point for the orchestrator.
    /// It sequences: PRD Agent → Architect Agent → Ralph Loop Agent
    #[instrument(skip(self, prompt), fields(prompt_len = prompt.len()))]
    pub async fn run(&mut self, prompt: &str) -> Result<CompletionStatus> {
        info!(prompt = prompt, "Starting Ralph pipeline");
        let _timing = start_timing("full_pipeline");

        // Phase 1: Requirements
        self.output.phase("Phase 1: Requirements Generation");
        self.output.status("Analyzing project description...");
        
        let prd = self.run_requirements_phase(prompt).await?;
        
        // Show user stories summary
        self.output.phase_complete(&format!(
            "Generated {} user stories:",
            prd.user_stories.len()
        ));
        for story in &prd.user_stories {
            self.output.list_item(&format!("{}: {}", story.id, story.title));
        }
        info!(
            user_stories = prd.user_stories.len(),
            "Requirements phase complete"
        );

        // Phase 2: Design
        self.output.phase("Phase 2: Design & Task Breakdown");
        self.output.status("Creating system architecture...");
        
        let (design, tasks) = self.run_design_phase().await?;
        
        // Show tasks summary
        self.output.phase_complete(&format!(
            "Created {} components, {} tasks:",
            design.components.len(),
            tasks.get_stats().total
        ));
        for task in &tasks.tasks {
            self.output.list_item(&format!("{}: {}", task.id, task.title));
        }
        info!(
            components = design.components.len(),
            tasks = tasks.get_stats().total,
            "Design phase complete"
        );

        // Phase 3: Implementation
        self.output.phase("Phase 3: Implementation");
        self.output.status("Starting task implementation loop...");
        
        let status = self.run_implementation_phase().await?;
        
        info!(status = %status, "Implementation phase complete");

        Ok(status)
    }

    /// Resume the pipeline from the current phase.
    ///
    /// This is useful for resuming after a failure or interruption.
    pub async fn resume(&mut self, prompt: &str) -> Result<CompletionStatus> {
        info!(phase = %self.state.phase, "Resuming pipeline");

        match self.state.phase {
            PipelinePhase::Requirements => self.run(prompt).await,
            PipelinePhase::Design => {
                // Load PRD if not in state
                if self.state.prd.is_none() {
                    let prd_path = self.project_path.join(&self.config.prd_path);
                    let prd = PrdDocument::load_markdown(&prd_path)
                        .map_err(RalphError::Prd)?;
                    self.state.prd = Some(prd);
                }
                
                let (_, _) = self.run_design_phase().await?;
                self.run_implementation_phase().await
            }
            PipelinePhase::Implementation => {
                // Load design and tasks if not in state
                if self.state.design.is_none() {
                    let design_path = self.project_path.join(&self.config.design_path);
                    let design = DesignDocument::load_markdown(&design_path)
                        .map_err(RalphError::Design)?;
                    self.state.design = Some(design);
                }
                if self.state.tasks.is_none() {
                    let tasks_path = self.project_path.join(&self.config.tasks_path);
                    let tasks = TaskList::load(&tasks_path)
                        .map_err(RalphError::Task)?;
                    self.state.tasks = Some(tasks);
                }
                
                self.run_implementation_phase().await
            }
            PipelinePhase::Complete => {
                // Already complete, return the status
                self.state.completion_status.clone().ok_or_else(|| {
                    RalphError::Internal("Pipeline complete but no status available".to_string())
                })
            }
        }
    }

    /// Skip to a specific phase.
    ///
    /// This is useful for testing or when you want to start from a specific phase
    /// with pre-existing artifacts.
    pub fn skip_to_phase(&mut self, phase: PipelinePhase) -> Result<()> {
        match phase {
            PipelinePhase::Requirements => {
                self.state.phase = PipelinePhase::Requirements;
            }
            PipelinePhase::Design => {
                // Verify PRD exists
                if !self.prd_exists() {
                    return Err(RalphError::Prd(
                        "Cannot skip to Design phase: PRD file does not exist".to_string(),
                    ));
                }
                self.state.phase = PipelinePhase::Design;
            }
            PipelinePhase::Implementation => {
                // Verify design and tasks exist
                if !self.design_exists() || !self.tasks_exist() {
                    return Err(RalphError::Design(
                        "Cannot skip to Implementation phase: Design or tasks file does not exist"
                            .to_string(),
                    ));
                }
                self.state.phase = PipelinePhase::Implementation;
            }
            PipelinePhase::Complete => {
                return Err(RalphError::Internal(
                    "Cannot skip to Complete phase".to_string(),
                ));
            }
        }
        Ok(())
    }
}


/// Builder for creating a RalphOrchestrator with fluent API.
#[derive(Debug, Clone, Default)]
pub struct OrchestratorBuilder {
    config: Option<RalphConfig>,
    project_path: Option<PathBuf>,
}

impl OrchestratorBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the configuration.
    pub fn config(mut self, config: RalphConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Set the project path.
    pub fn project_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.project_path = Some(path.into());
        self
    }

    /// Build the orchestrator.
    pub fn build(self) -> Result<RalphOrchestrator> {
        let mut config = self.config.unwrap_or_default();

        // Override project path if specified
        if let Some(path) = self.project_path {
            config.project_path = path.to_string_lossy().to_string();
        }

        RalphOrchestrator::new(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipeline_phase_display() {
        assert_eq!(PipelinePhase::Requirements.to_string(), "Requirements");
        assert_eq!(PipelinePhase::Design.to_string(), "Design");
        assert_eq!(PipelinePhase::Implementation.to_string(), "Implementation");
        assert_eq!(PipelinePhase::Complete.to_string(), "Complete");
    }

    #[test]
    fn test_orchestrator_state_default() {
        let state = OrchestratorState::default();
        assert_eq!(state.phase, PipelinePhase::Requirements);
        assert!(state.prd.is_none());
        assert!(state.design.is_none());
        assert!(state.tasks.is_none());
        assert!(state.completion_status.is_none());
    }

    #[test]
    fn test_orchestrator_builder() {
        let config = RalphConfig::default();
        let orchestrator = OrchestratorBuilder::new()
            .config(config)
            .project_path("/tmp/test")
            .build()
            .unwrap();

        assert_eq!(orchestrator.phase(), PipelinePhase::Requirements);
        assert_eq!(
            orchestrator.project_path().to_string_lossy(),
            "/tmp/test"
        );
    }

    #[test]
    fn test_orchestrator_new() {
        let config = RalphConfig::default();
        let orchestrator = RalphOrchestrator::new(config).unwrap();

        assert_eq!(orchestrator.phase(), PipelinePhase::Requirements);
    }

    #[test]
    fn test_skip_to_phase_validation() {
        let config = RalphConfig::default();
        let mut orchestrator = RalphOrchestrator::new(config).unwrap();

        // Should fail to skip to Design without PRD
        let result = orchestrator.skip_to_phase(PipelinePhase::Design);
        assert!(result.is_err());

        // Should fail to skip to Implementation without design/tasks
        let result = orchestrator.skip_to_phase(PipelinePhase::Implementation);
        assert!(result.is_err());

        // Should fail to skip to Complete
        let result = orchestrator.skip_to_phase(PipelinePhase::Complete);
        assert!(result.is_err());

        // Should succeed to skip to Requirements
        let result = orchestrator.skip_to_phase(PipelinePhase::Requirements);
        assert!(result.is_ok());
    }
}
