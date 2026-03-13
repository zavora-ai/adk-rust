use adk_model::ModelProvider;
use clap::{Parser, Subcommand, ValueEnum};

/// ADK-Rust CLI — chat with an AI agent, serve a web UI, or manage skills.
///
/// Running `adk-rust` with no subcommand starts an interactive REPL.
/// On first run, if no API key is configured, you'll be prompted to
/// select a provider and enter your key.
///
/// For custom agents, use [`adk_cli::Launcher`] in your own binary.
#[derive(Parser)]
#[command(name = "adk-rust")]
#[command(about = "ADK-Rust CLI — interactive agent, web server, and skill tooling")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// LLM provider
    #[arg(long, global = true)]
    pub provider: Option<ModelProvider>,

    /// Model name (provider-specific, uses sensible default if omitted)
    #[arg(long, global = true)]
    pub model: Option<String>,

    /// API key — overrides saved config and env vars
    #[arg(long, global = true)]
    pub api_key: Option<String>,

    /// Agent instruction / system prompt
    #[arg(long, global = true)]
    pub instruction: Option<String>,

    /// Request provider-side thinking mode with the given token budget when supported.
    #[arg(long, global = true)]
    pub thinking_budget: Option<u32>,

    /// How the CLI should render emitted thinking/reasoning content.
    #[arg(long, global = true, value_enum, default_value_t = ThinkingMode::Auto)]
    pub thinking_mode: ThinkingMode,
}

/// All providers in menu order.
pub const ALL_PROVIDERS: &[ModelProvider] = ModelProvider::all();

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ThinkingMode {
    Auto,
    Show,
    Hide,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Interactive REPL with an AI agent (this is the default)
    Chat,

    /// Start web server with an AI agent
    Serve {
        /// Server port
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },

    /// Skills tooling (list/validate/match)
    Skills {
        #[command(subcommand)]
        command: SkillsCommands,
    },

    /// Deployment platform commands
    Deploy {
        #[command(subcommand)]
        command: DeployCommands,
    },
}

#[derive(Subcommand, Clone)]
pub enum SkillsCommands {
    /// List indexed skills
    List {
        /// Project root containing .skills/
        #[arg(long, default_value = ".")]
        path: String,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Validate skill files under .skills/
    Validate {
        /// Project root containing .skills/
        #[arg(long, default_value = ".")]
        path: String,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Match skills against a query
    Match {
        /// Query text to rank against skill metadata/content
        #[arg(long)]
        query: String,
        /// Project root containing .skills/
        #[arg(long, default_value = ".")]
        path: String,
        /// Maximum number of matched skills to return
        #[arg(long, default_value_t = 3)]
        top_k: usize,
        /// Minimum score threshold
        #[arg(long, default_value_t = 1.0)]
        min_score: f32,
        /// Include only skills containing at least one of these tags
        #[arg(long = "include-tag")]
        include_tags: Vec<String>,
        /// Exclude skills containing any of these tags
        #[arg(long = "exclude-tag")]
        exclude_tags: Vec<String>,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

#[derive(Subcommand, Clone)]
pub enum DeployCommands {
    /// Authenticate against the deployment control plane
    Login {
        #[arg(long, default_value = "http://127.0.0.1:8090")]
        endpoint: String,
        #[arg(long)]
        token: String,
    },
    /// Remove locally stored deployment credentials
    Logout,
    /// Create a starter deployment manifest in the current project
    Init {
        #[arg(long, default_value = "adk-deploy.toml")]
        path: String,
        #[arg(long)]
        agent_name: Option<String>,
        #[arg(long)]
        binary: Option<String>,
    },
    /// Validate a deployment manifest without contacting the control plane
    Validate {
        #[arg(long, default_value = "adk-deploy.toml")]
        path: String,
    },
    /// Build a deployment bundle
    Build {
        #[arg(long, default_value = "adk-deploy.toml")]
        path: String,
    },
    /// Push a deployment bundle to an environment
    Push {
        #[arg(long, default_value = "adk-deploy.toml")]
        path: String,
        #[arg(long, default_value = "staging")]
        env: String,
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Show the latest deployment status
    Status {
        #[arg(long, default_value = "production")]
        env: String,
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show deployment history
    History {
        #[arg(long, default_value = "production")]
        env: String,
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show the latest metrics summary
    Metrics {
        #[arg(long, default_value = "production")]
        env: String,
        #[arg(long)]
        agent: Option<String>,
    },
    /// Roll back from a deployment id
    Rollback {
        #[arg(long)]
        deployment_id: String,
    },
    /// Promote a canary deployment
    Promote {
        #[arg(long)]
        deployment_id: String,
    },
    /// Manage environment secrets
    Secret {
        #[command(subcommand)]
        command: DeploySecretCommands,
    },
}

#[derive(Subcommand, Clone)]
pub enum DeploySecretCommands {
    /// Set or overwrite a secret
    Set {
        #[arg(long)]
        env: String,
        key: String,
        value: String,
    },
    /// List secret keys for an environment
    List {
        #[arg(long)]
        env: String,
    },
    /// Delete a secret key
    Delete {
        #[arg(long)]
        env: String,
        key: String,
    },
}
