//! Ralph - Multi-Agent Autonomous Development System
//!
//! This is the main entry point for the Ralph CLI.
//!
//! ## Usage
//!
//! ```bash
//! # Run with a project description
//! ralph "Create a CLI calculator in Rust"
//!
//! # Run with environment configuration
//! RALPH_MODEL_PROVIDER=anthropic ralph "Build a REST API"
//! ```

use adk_ralph::{DebugLevel, PipelinePhase, RalphConfig, RalphOrchestrator, RalphOutput, Result, TelemetryConfig};
use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use tracing::info;

/// Ralph - Multi-Agent Autonomous Development System
#[derive(Parser, Debug)]
#[command(name = "ralph")]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Subcommand to run
    #[command(subcommand)]
    command: Option<Commands>,

    /// Output verbosity level
    #[arg(short = 'd', long, value_enum, global = true)]
    debug: Option<CliDebugLevel>,

    /// Project output directory (overrides RALPH_PROJECT_PATH)
    #[arg(short = 'p', long, global = true)]
    project_path: Option<String>,

    /// Project description (when no subcommand is used)
    #[arg(trailing_var_arg = true)]
    prompt: Vec<String>,
}

/// CLI debug level (maps to DebugLevel)
#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliDebugLevel {
    /// Only errors and final status
    Minimal,
    /// Human-readable progress (default)
    Normal,
    /// Detailed output with tool calls
    Verbose,
    /// Full debug output
    Debug,
}

impl From<CliDebugLevel> for DebugLevel {
    fn from(cli: CliDebugLevel) -> Self {
        match cli {
            CliDebugLevel::Minimal => DebugLevel::Minimal,
            CliDebugLevel::Normal => DebugLevel::Normal,
            CliDebugLevel::Verbose => DebugLevel::Verbose,
            CliDebugLevel::Debug => DebugLevel::Debug,
        }
    }
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the full pipeline from prompt to completion
    Run {
        /// Project description
        #[arg(required = true)]
        prompt: Vec<String>,
    },
    /// Resume from a specific phase
    Resume {
        /// Phase to resume from (requirements, design, implementation)
        #[arg(short, long, default_value = "requirements")]
        phase: String,
        /// Project description (required for requirements phase)
        prompt: Vec<String>,
    },
    /// Show current status
    Status,
    /// Validate configuration
    Config,
}

/// Initialize telemetry based on configuration and debug level.
fn init_telemetry(config: &TelemetryConfig, debug_level: DebugLevel) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    
    // Determine log level based on debug level
    let log_level = match debug_level {
        DebugLevel::Minimal | DebugLevel::Normal => "error", // Suppress INFO logs for clean output
        DebugLevel::Verbose => "warn",
        DebugLevel::Debug => &config.log_level, // Use configured level for debug mode
    };
    
    // For minimal/normal, we want clean output without tracing logs
    if matches!(debug_level, DebugLevel::Minimal | DebugLevel::Normal) {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(log_level));
        tracing_subscriber::registry()
            .with(fmt::layer().with_target(false))
            .with(filter)
            .init();
        return Ok(());
    }

    if !config.enabled {
        // If telemetry is disabled, just set up basic logging
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(log_level));
        tracing_subscriber::registry()
            .with(fmt::layer().with_target(false))
            .with(filter)
            .init();
        return Ok(());
    }

    // Use adk-telemetry for full telemetry support (verbose/debug modes)
    if let Some(ref endpoint) = config.otlp_endpoint {
        // Initialize with OTLP export for distributed tracing and metrics
        adk_telemetry::init_with_otlp(&config.service_name, endpoint)?;
    } else {
        // Initialize basic telemetry with console logging
        adk_telemetry::init_telemetry(&config.service_name)?;
    }

    Ok(())
}

fn _print_banner() {
    println!(
        "{}",
        r#"
  ____       _       _     
 |  _ \ __ _| |_ __ | |__  
 | |_) / _` | | '_ \| '_ \ 
 |  _ < (_| | | |_) | | | |
 |_| \_\__,_|_| .__/|_| |_|
              |_|          
"#
        .cyan()
    );
    println!(
        "{}",
        "Multi-Agent Autonomous Development System".bright_white()
    );
    println!();
}

fn print_config(config: &RalphConfig) {
    println!("{}", "Configuration:".yellow().bold());
    println!(
        "  PRD Agent:       {} ({}) {}",
        config.agents.prd_model.provider.cyan(),
        config.agents.prd_model.model_name,
        if config.agents.prd_model.thinking_enabled {
            "[thinking]".green()
        } else {
            "".normal()
        }
    );
    println!(
        "  Architect Agent: {} ({}) {}",
        config.agents.architect_model.provider.cyan(),
        config.agents.architect_model.model_name,
        if config.agents.architect_model.thinking_enabled {
            "[thinking]".green()
        } else {
            "".normal()
        }
    );
    println!(
        "  Ralph Loop:      {} ({})",
        config.agents.ralph_model.provider.cyan(),
        config.agents.ralph_model.model_name
    );
    println!("  Max Iterations:  {}", config.max_iterations);
    println!("  Debug Level:     {}", config.debug_level.to_string().cyan());
    println!("  Project Path:    {}", config.project_path);
    println!();
}

fn print_status(orchestrator: &RalphOrchestrator) {
    println!("{}", "Pipeline Status:".yellow().bold());
    println!("  Current Phase: {}", orchestrator.phase().to_string().cyan());
    println!();

    // Check for existing artifacts
    println!("{}", "Artifacts:".yellow().bold());
    
    let prd_status = if orchestrator.prd_exists() {
        "✓ exists".green()
    } else {
        "✗ not found".red()
    };
    println!("  PRD ({}): {}", orchestrator.config().prd_path, prd_status);

    let design_status = if orchestrator.design_exists() {
        "✓ exists".green()
    } else {
        "✗ not found".red()
    };
    println!(
        "  Design ({}): {}",
        orchestrator.config().design_path,
        design_status
    );

    let tasks_status = if orchestrator.tasks_exist() {
        "✓ exists".green()
    } else {
        "✗ not found".red()
    };
    println!(
        "  Tasks ({}): {}",
        orchestrator.config().tasks_path,
        tasks_status
    );

    println!();
}

fn parse_phase(phase_str: &str) -> Result<PipelinePhase> {
    match phase_str.to_lowercase().as_str() {
        "requirements" | "req" | "prd" => Ok(PipelinePhase::Requirements),
        "design" | "arch" | "architect" => Ok(PipelinePhase::Design),
        "implementation" | "impl" | "loop" => Ok(PipelinePhase::Implementation),
        _ => Err(adk_ralph::RalphError::Configuration(format!(
            "Unknown phase: {}. Valid phases: requirements, design, implementation",
            phase_str
        ))),
    }
}

async fn run_pipeline(config: RalphConfig, prompt: &str) -> Result<()> {
    let mut orchestrator = RalphOrchestrator::new(config)?;

    println!("{}", "Starting Ralph Pipeline...".green().bold());
    println!();

    // Run the full pipeline
    let status = orchestrator.run(prompt).await?;

    // Print final status
    println!();
    println!("{}", "Pipeline Complete!".green().bold());
    println!("{}", status);

    Ok(())
}

async fn resume_pipeline(config: RalphConfig, phase: PipelinePhase, prompt: &str) -> Result<()> {
    let mut orchestrator = RalphOrchestrator::new(config)?;

    // Skip to the specified phase
    orchestrator.skip_to_phase(phase)?;

    println!(
        "{} {}",
        "Resuming from phase:".green().bold(),
        phase.to_string().cyan()
    );
    println!();

    // Resume the pipeline
    let status = orchestrator.resume(prompt).await?;

    // Print final status
    println!();
    println!("{}", "Pipeline Complete!".green().bold());
    println!("{}", status);

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file from current directory
    match dotenvy::dotenv() {
        Ok(path) => {
            // .env loaded successfully
            eprintln!("Loaded config from: {}", path.display());
        }
        Err(_) => {
            // No .env file found - that's okay, will use env vars or defaults
        }
    }

    // Parse command line arguments
    let cli = Cli::parse();

    // Load configuration
    let mut config = match RalphConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            let cwd = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| ".".to_string());
            
            eprintln!("{}: {}", "Configuration Error".red().bold(), e);
            eprintln!();
            eprintln!("Create a {} file at:", ".env".cyan());
            eprintln!("  {}", format!("{}/.env", cwd).yellow());
            eprintln!();
            eprintln!("With contents:");
            eprintln!("  # Required: API key for your provider");
            eprintln!("  GEMINI_API_KEY=your-api-key");
            eprintln!("  # or ANTHROPIC_API_KEY=your-api-key");
            eprintln!("  # or OPENAI_API_KEY=your-api-key");
            eprintln!();
            eprintln!("  # Optional: customize project output location");
            eprintln!("  RALPH_PROJECT_PATH=/path/to/project");
            eprintln!();
            eprintln!("See {} for all options.", "adk-ralph/.env.example".cyan());
            std::process::exit(1);
        }
    };

    // Override from CLI if provided
    if let Some(debug_level) = cli.debug {
        config.debug_level = debug_level.into();
    }
    if let Some(ref path) = cli.project_path {
        config.project_path = path.clone();
    }

    // Initialize telemetry
    if let Err(e) = init_telemetry(&config.telemetry, config.debug_level) {
        eprintln!("{}: {}", "Telemetry Warning".yellow(), e);
        eprintln!("Continuing without full telemetry support...");
    }

    // Create output handler for banner (respects debug level)
    let output = RalphOutput::new(config.debug_level);

    // Print banner (only at normal and above)
    output.banner();

    // Handle commands
    match cli.command {
        Some(Commands::Run { prompt }) => {
            let prompt_str = prompt.join(" ");
            if prompt_str.is_empty() {
                eprintln!("{}", "Error: Project description is required".red());
                std::process::exit(1);
            }

            print_config(&config);
            info!("Starting Ralph with prompt: {}", prompt_str);
            println!("{} {}", "Project:".green().bold(), prompt_str);
            println!();

            run_pipeline(config, &prompt_str).await?;
        }

        Some(Commands::Resume { phase, prompt }) => {
            let phase = parse_phase(&phase)?;
            let prompt_str = prompt.join(" ");

            // Prompt is required for requirements phase
            if phase == PipelinePhase::Requirements && prompt_str.is_empty() {
                eprintln!(
                    "{}",
                    "Error: Project description is required for requirements phase".red()
                );
                std::process::exit(1);
            }

            print_config(&config);
            resume_pipeline(config, phase, &prompt_str).await?;
        }

        Some(Commands::Status) => {
            let orchestrator = RalphOrchestrator::new(config)?;
            print_status(&orchestrator);
        }

        Some(Commands::Config) => {
            print_config(&config);
            println!("{}", "Configuration is valid!".green());
        }

        None => {
            // No subcommand - use prompt directly
            let prompt_str = cli.prompt.join(" ");
            if prompt_str.is_empty() {
                eprintln!("{}", "Usage: ralph <project description>".yellow());
                eprintln!();
                eprintln!("Examples:");
                eprintln!("  ralph \"Create a CLI calculator in Rust\"");
                eprintln!("  ralph \"Build a REST API for a todo app in Python\"");
                eprintln!();
                eprintln!("Commands:");
                eprintln!("  ralph run <prompt>     Run the full pipeline");
                eprintln!("  ralph resume [--phase] Resume from a specific phase");
                eprintln!("  ralph status           Show current status");
                eprintln!("  ralph config           Validate configuration");
                std::process::exit(1);
            }

            print_config(&config);
            info!("Starting Ralph with prompt: {}", prompt_str);
            println!("{} {}", "Project:".green().bold(), prompt_str);
            println!();

            run_pipeline(config, &prompt_str).await?;
        }
    }

    // Shutdown telemetry to flush any pending spans
    adk_telemetry::shutdown_telemetry();

    Ok(())
}
