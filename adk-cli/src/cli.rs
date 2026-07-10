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

    /// Run the coding agent on a task in a workspace directory.
    ///
    /// The agent can read/edit files and run commands, sandboxed to the
    /// directory. Example: `adk-rust code "make the failing test pass"`
    Code {
        /// The task / instruction for the agent.
        task: String,

        /// Workspace directory the agent operates in (sandboxed).
        #[arg(short, long, default_value = ".")]
        dir: String,

        /// Explore/plan only: no file writes, no shell.
        #[arg(long)]
        read_only: bool,
    },

    /// Autonomous goal mode: work until a verifiable success condition passes.
    ///
    /// The agent loops plan → act → verify, self-correcting from the check's
    /// output, until `--until` exits 0 or the iteration budget is reached.
    /// Example: `adk-rust goal "make all tests pass" --until "cargo test"`
    Goal {
        /// The high-level goal.
        goal: String,

        /// Shell command whose exit code 0 means "goal met" (the success condition).
        #[arg(long)]
        until: String,

        /// Workspace directory the agent operates in (sandboxed).
        #[arg(short, long, default_value = ".")]
        dir: String,

        /// Maximum number of plan→act→verify iterations (budget).
        #[arg(long, default_value_t = 8)]
        max_iters: u32,

        /// Path to the durable goal-state file (default: `<dir>/.adk/goal.json`).
        #[arg(long)]
        state: Option<String>,

        /// Resume from a saved goal state instead of starting fresh.
        #[arg(long)]
        resume: bool,
    },

    /// Ultracode: fan out to parallel specialist reviewers and iterate.
    ///
    /// Implements the task, then runs correctness/edge-case/style reviewers in
    /// parallel, synthesizes their verdicts, and revises until they approve.
    /// Example: `adk-rust ultracode "add a /health endpoint"`
    Ultracode {
        /// The task to implement and review.
        task: String,

        /// Workspace directory the agent operates in (sandboxed).
        #[arg(short, long, default_value = ".")]
        dir: String,

        /// Maximum number of review→revise rounds.
        #[arg(long, default_value_t = 2)]
        max_rounds: i64,
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

    /// Graph time-travel debugging commands
    Graph {
        #[command(subcommand)]
        command: GraphCommands,
    },
}

#[derive(Subcommand, Clone)]
pub enum GraphCommands {
    /// List all checkpoints for a thread
    Steps {
        /// Thread identifier to inspect
        thread_id: String,

        /// Path to a SQLite checkpoint database
        #[arg(long)]
        db: Option<String>,
    },

    /// Replay execution between two steps, printing state transitions
    Replay {
        /// Thread identifier to replay
        thread_id: String,

        /// Step number to start replaying from
        #[arg(long)]
        from: usize,

        /// Step number to stop at (inclusive). Omit to replay to the last step.
        #[arg(long)]
        to: Option<usize>,

        /// Path to a SQLite checkpoint database
        #[arg(long)]
        db: Option<String>,
    },

    /// Fork execution at a step into a new thread
    Fork {
        /// Thread identifier to fork from
        thread_id: String,

        /// Step number to fork at
        #[arg(long)]
        at: usize,

        /// New thread identifier for the forked execution
        #[arg(long)]
        new_thread: String,

        /// Path to a SQLite checkpoint database
        #[arg(long)]
        db: Option<String>,
    },

    /// Resume execution from a specific step
    Resume {
        /// Thread identifier to resume
        thread_id: String,

        /// Step number to resume from
        #[arg(long)]
        from: usize,

        /// Path to a SQLite checkpoint database
        #[arg(long)]
        db: Option<String>,
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
