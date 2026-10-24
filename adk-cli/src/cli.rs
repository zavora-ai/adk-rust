use adk_model::ModelProvider;
use clap::{Parser, Subcommand, ValueEnum};

/// A provider this binary runs on: one adk-model talks to, or mistral.rs in process
/// (spec 3.1 §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    /// A provider adk-model talks to.
    Model(ModelProvider),
    /// mistral.rs in this process (feature `mistralrs`): `--model` names a Hugging Face
    /// model id or a `.gguf` path.
    MistralRs,
}

impl Provider {
    /// Every provider in menu order: adk-model's, then mistral.rs.
    pub const ALL: [Provider; 7] = [
        Provider::Model(ModelProvider::Gemini),
        Provider::Model(ModelProvider::Openai),
        Provider::Model(ModelProvider::Anthropic),
        Provider::Model(ModelProvider::Deepseek),
        Provider::Model(ModelProvider::Groq),
        Provider::Model(ModelProvider::Ollama),
        Provider::MistralRs,
    ];

    /// Every provider in menu order.
    pub const fn all() -> &'static [Provider] {
        &Self::ALL
    }

    /// The identifier `--provider` and the saved config use.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Model(provider) => provider.as_str(),
            Self::MistralRs => "mistralrs",
        }
    }

    /// The default model; empty for mistral.rs, which needs `--model`.
    pub const fn default_model(self) -> &'static str {
        match self {
            Self::Model(provider) => provider.default_model(),
            Self::MistralRs => "",
        }
    }

    /// The environment variable holding the API key; empty for local providers.
    pub const fn env_var(self) -> &'static str {
        match self {
            Self::Model(provider) => provider.env_var(),
            Self::MistralRs => "",
        }
    }

    /// An alternate environment variable for the API key.
    pub const fn alt_env_var(self) -> Option<&'static str> {
        match self {
            Self::Model(provider) => provider.alt_env_var(),
            Self::MistralRs => None,
        }
    }

    /// Whether the provider needs an API key.
    pub const fn requires_key(self) -> bool {
        match self {
            Self::Model(provider) => provider.requires_key(),
            Self::MistralRs => false,
        }
    }

    /// Whether the provider runs on this machine, so nothing leaves it but the model
    /// endpoint (spec 3.1 §8.1, H10).
    #[cfg(feature = "harness")]
    pub const fn is_local(self) -> bool {
        matches!(self, Self::Model(ModelProvider::Ollama) | Self::MistralRs)
    }

    /// The name shown in prompts and help.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Model(provider) => provider.display_name(),
            Self::MistralRs => "mistral.rs (local, in process; --model <hf id or .gguf path>)",
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Provider {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "mistralrs" | "mistral-rs" | "mistral.rs" => Ok(Self::MistralRs),
            other => other.parse::<ModelProvider>().map(Self::Model),
        }
    }
}

/// ADK-Rust CLI — the governed harness in your terminal, a web server, and agent tooling.
///
/// Running `adk-rust` with no subcommand starts the harness REPL in the current directory:
/// it reads, edits and runs code in the workspace under approvals (spec 3.1 §8.1).
/// On first run, if no API key is configured, you'll be prompted to
/// select a provider and enter your key.
///
/// For custom agents, use [`adk_cli::Launcher`] in your own binary.
#[derive(Parser)]
#[command(name = "adk-rust")]
#[command(
    about = "ADK-Rust CLI — governed coding harness, interactive agent, web server, and tooling"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    #[command(flatten)]
    pub options: AgentOptions,

    /// Harness flags for the REPL that runs without a subcommand.
    #[cfg(feature = "harness")]
    #[command(flatten)]
    pub harness: adk_cli::harness::HarnessArgs,

    /// The voice surface (`--voice`), which also runs without a subcommand.
    #[cfg(feature = "voice")]
    #[command(flatten)]
    pub voice: VoiceArgs,
}

/// `adk-rust --voice`: the harness over a realtime session (spec 3.1 §8.1, §7.17, H5).
#[cfg(feature = "voice")]
#[derive(clap::Args, Debug, Clone, Default)]
pub struct VoiceArgs {
    /// Talk to the harness: voice in and out, spoken approvals, the same policy. TEXT is
    /// the first turn. Needs --provider gemini or openai.
    #[arg(long, value_name = "TEXT", num_args = 0..=1, default_missing_value = "")]
    pub voice: Option<String>,

    /// Without a microphone: a mono PCM16 WAV (16 kHz for Gemini, 24 kHz for OpenAI) sent as
    /// the answer to the first held call; the run ends after that call's response (exit 3
    /// while a call is still held).
    #[arg(long, value_name = "WAV", requires = "voice")]
    pub voice_input: Option<std::path::PathBuf>,

    /// Write the model's audio to this WAV file instead of playing it.
    #[arg(long, value_name = "WAV", requires = "voice")]
    pub voice_output: Option<std::path::PathBuf>,

    /// A scripted realtime provider instead of a live one: a JSON array of scripted steps,
    /// inline or in the named file (end-to-end tests).
    #[cfg(feature = "scripted-model")]
    #[arg(long, value_name = "JSON", hide = true, requires = "voice")]
    pub voice_script: Option<String>,
}

/// Model and rendering options shared by every agent command.
#[derive(clap::Args, Debug, Clone)]
pub struct AgentOptions {
    /// LLM provider: gemini, openai, anthropic, deepseek, groq, ollama or mistralrs
    #[arg(long, global = true)]
    pub provider: Option<Provider>,

    /// Model name (provider-specific, uses sensible default if omitted)
    #[arg(long, global = true)]
    pub model: Option<String>,

    /// API key — overrides saved config and env vars
    #[arg(long, global = true)]
    pub api_key: Option<String>,

    /// Agent instruction / system prompt. For `code`, `goal` and `ultracode` it is added
    /// to the coding prompt as project guidance.
    #[arg(long, global = true)]
    pub instruction: Option<String>,

    /// Request provider-side thinking mode with the given token budget when supported.
    #[arg(long, global = true)]
    pub thinking_budget: Option<u32>,

    /// How the CLI should render emitted thinking/reasoning content.
    #[arg(long, global = true, value_enum, default_value_t = ThinkingMode::Auto)]
    pub thinking_mode: ThinkingMode,

    /// Answer model calls from a JSON script instead of a provider (end-to-end tests).
    ///
    /// A JSON array, inline or in the named file, of `{"text": "..."}`,
    /// `{"call": {"name": "...", "args": {...}}}` and `{"calls": [...]}` replies.
    #[cfg(feature = "scripted-model")]
    #[arg(long, global = true, hide = true)]
    pub model_script: Option<String>,

    /// Give the chat agent the `record_note` test tool, which needs approval.
    #[cfg(feature = "scripted-model")]
    #[arg(long, global = true, hide = true)]
    pub approval_fixture: bool,
}

/// The providers compiled into this build, in menu order (spec 3.1 §8.1).
pub fn compiled_providers() -> Vec<Provider> {
    Provider::all().iter().copied().filter(|provider| is_compiled(*provider)).collect()
}

/// Whether this build can talk to `provider`: its Cargo feature is enabled.
pub fn is_compiled(provider: Provider) -> bool {
    match provider {
        Provider::Model(ModelProvider::Gemini) => cfg!(feature = "gemini"),
        Provider::Model(ModelProvider::Openai) => cfg!(feature = "openai"),
        Provider::Model(ModelProvider::Anthropic) => cfg!(feature = "anthropic"),
        Provider::Model(ModelProvider::Deepseek) => cfg!(feature = "deepseek"),
        Provider::Model(ModelProvider::Groq) => cfg!(feature = "groq"),
        Provider::Model(ModelProvider::Ollama) => cfg!(feature = "ollama"),
        Provider::MistralRs => cfg!(feature = "mistralrs"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ThinkingMode {
    Auto,
    Show,
    Hide,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Interactive REPL with the plain chat agent (no workspace tools)
    Chat(ChatArgs),

    /// Serve the harness in plan mode with the approval inbox, on 127.0.0.1 unless --host
    /// says otherwise
    Serve {
        /// Server port
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// Address to listen on; anything but loopback needs --auth-token
        #[arg(long, default_value = "127.0.0.1")]
        host: std::net::IpAddr,
        /// What to serve: the harness (default) or the plain chat agent
        #[cfg(feature = "harness")]
        #[arg(long, value_enum, default_value_t = ServeAgent::Harness)]
        agent: ServeAgent,
        /// Bearer token callers must present (default: $ADK_SERVE_TOKEN); required beyond
        /// loopback
        #[cfg(feature = "harness")]
        #[arg(long)]
        auth_token: Option<String>,
        /// Keep sessions, approvals, runs, modes, standing rules and plans in one store every
        /// replica shares: postgres://… or redis://… (default: harness.db; spec 3.1 §8.2)
        #[cfg(feature = "harness")]
        #[arg(long, value_name = "URL", conflicts_with = "ephemeral")]
        store: Option<String>,
        #[cfg(feature = "harness")]
        #[command(flatten)]
        harness: adk_cli::harness::HarnessArgs,
    },

    /// Run one harness turn on a task in a workspace directory.
    ///
    /// The agent reads and edits files in the workspace and runs commands after you approve
    /// them. Exit code 3 means calls are held for a decision; the session id is printed.
    /// Example: `adk-rust code "make the failing test pass"`
    Code {
        /// The task / instruction for the agent.
        task: String,

        #[cfg(feature = "harness")]
        #[command(flatten)]
        harness: adk_cli::harness::HarnessArgs,

        /// Workspace directory the agent operates in. File tools are confined to it;
        /// `bash` runs on the host with the environment cleared, not in an OS sandbox.
        #[cfg(not(feature = "harness"))]
        #[arg(short, long, default_value = ".")]
        dir: String,

        /// Explore/plan only: no file writes, no shell (`--mode read-only`).
        #[arg(long, hide = cfg!(feature = "harness"))]
        read_only: bool,
    },

    /// Autonomous goal mode: work until a verifiable success condition passes.
    ///
    /// The agent loops plan → act → verify, self-correcting from the check's
    /// output, until `--until` exits 0 or the iteration budget is reached. The goal and its
    /// conversation are kept in the session, so `--resume` continues both.
    /// Example: `adk-rust goal "make all tests pass" --until "cargo test"`
    Goal {
        /// The high-level goal.
        goal: String,

        /// Shell command whose exit code 0 means "goal met" (the success condition).
        #[arg(long)]
        until: String,

        #[cfg(feature = "harness")]
        #[command(flatten)]
        harness: adk_cli::harness::HarnessArgs,

        /// Workspace directory the agent operates in. File tools are confined to it;
        /// `bash` runs on the host with the environment cleared, not in an OS sandbox.
        #[cfg(not(feature = "harness"))]
        #[arg(short, long, default_value = ".")]
        dir: String,

        /// Maximum number of plan→act→verify iterations (budget).
        #[arg(long, default_value_t = 8)]
        max_iters: u32,

        /// Seconds one run of the `--until` check may take before it counts as failed
        #[cfg(feature = "harness")]
        #[arg(long, value_name = "SECS", default_value_t = 600)]
        check_timeout: u64,

        /// Path of a JSON mirror of the goal state (default without the harness:
        /// `<dir>/.adk/goal.json`).
        #[arg(long, hide = cfg!(feature = "harness"))]
        state: Option<String>,

        /// Resume the latest unfinished goal in this workspace instead of starting fresh.
        #[arg(long)]
        resume: bool,
    },

    /// Ultracode: implement, then review with specialist reviewers and iterate.
    ///
    /// Implements the task in your mode, then routes the review once, on the change: parallel
    /// independent reviewers (graph) or one board whose reviewers read each other
    /// (blackboard), all read-only. It synthesizes their verdicts and revises until they
    /// approve, within a model-call budget. Example: `adk-rust ultracode "add a /health
    /// endpoint"`
    Ultracode {
        /// The task to implement and review.
        task: String,

        #[cfg(feature = "harness")]
        #[command(flatten)]
        harness: adk_cli::harness::HarnessArgs,

        /// Workspace directory the agent operates in. File tools are confined to it;
        /// `bash` runs on the host with the environment cleared, not in an OS sandbox.
        #[cfg(not(feature = "harness"))]
        #[arg(short, long, default_value = ".")]
        dir: String,

        /// Maximum number of review→revise rounds (default 2; at most 5 with the harness)
        #[arg(long, value_name = "N")]
        max_rounds: Option<i64>,

        /// Review architecture: auto (routed on the change), graph (parallel reviewers) or
        /// blackboard (one board); default auto
        #[cfg(feature = "harness")]
        #[arg(long, value_enum, value_name = "ARCH")]
        architecture: Option<adk_cli::harness::ultracode::ArchitectureChoice>,

        /// How auto chooses: model (one closed-schema call, the heuristic when it fails) or
        /// heuristic; default model
        #[cfg(feature = "harness")]
        #[arg(long, value_enum, value_name = "MODE")]
        router: Option<adk_cli::harness::ultracode::RouterMode>,

        /// Most model calls of the whole run (default 400, at most 2000)
        #[cfg(feature = "harness")]
        #[arg(long, value_name = "N")]
        max_model_calls: Option<u64>,

        /// Write the run's audit records, their chain verification and the router's raw
        /// answer to FILE
        #[cfg(feature = "harness")]
        #[arg(long, value_name = "FILE")]
        receipt: Option<std::path::PathBuf>,
    },

    /// Run the harness as the agent kernel, in the foreground or as a service: a worker per
    /// queue, wake sources, the process table, and a control plane on a local socket (a
    /// named pipe on Windows) and HTTP.
    ///
    /// Example: `adk-rust up --mode ask`, then `adk-rust ps` or `adk-rust attach` from
    /// another terminal; `adk-rust up --mode ask --install` keeps it running as a service
    /// (spec 3.2 §8.1, §7.12).
    #[cfg(feature = "harness")]
    Up(Box<UpArgs>),

    /// Open the REPL on the running kernel: each turn runs as a process the kernel owns,
    /// held calls are answered here or from any other surface, and closing the terminal
    /// changes nothing.
    #[cfg(feature = "harness")]
    Attach {
        /// Continue this session (default: a new one)
        #[arg(long)]
        session: Option<String>,
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// The running kernel's processes, each with its age, spend, calls and goal progress
    #[cfg(feature = "harness")]
    Ps {
        /// Only processes in this partition
        #[arg(long)]
        partition: Option<String>,
        /// Only processes in this state: queued, running, waiting, completed, failed or
        /// cancelled
        #[arg(long, value_parser = parse_run_state)]
        state: Option<adk_core::RunState>,
        /// At most this many processes
        #[arg(long)]
        limit: Option<usize>,
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// `ps` ordered by burn rate, refreshing until Ctrl-C
    #[cfg(feature = "harness")]
    Top {
        /// Only processes in this partition
        #[arg(long)]
        partition: Option<String>,
        /// Seconds between refreshes
        #[arg(long, value_name = "SECS", default_value_t = 2)]
        interval: u64,
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// Kill a process: it stops at its next checkpoint and holds no lease
    #[cfg(feature = "harness")]
    Kill {
        /// Process id, or a unique prefix of it
        pid: String,
        /// Recorded on the run and in the audit record
        #[arg(long)]
        reason: Option<String>,
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// Suspend a process at its next model-call boundary
    #[cfg(feature = "harness")]
    Suspend {
        /// Process id, or a unique prefix of it
        pid: String,
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// Resume a suspended or sleeping process; one a freeze parked resumes on unfreeze
    #[cfg(feature = "harness")]
    Resume {
        /// Process id, or a unique prefix of it
        pid: String,
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// Freeze execution on every replica: running processes park and nothing starts
    #[cfg(feature = "harness")]
    Freeze {
        /// Why; shown with every refusal while frozen
        #[arg(long)]
        reason: Option<String>,
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// Lift the freeze: the processes it parked resume
    #[cfg(feature = "harness")]
    Unfreeze {
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// The kernel's event bus: process changes, wakes, holds, decisions, governance
    #[cfg(feature = "harness")]
    Events {
        /// Keep streaming new events until Ctrl-C
        #[arg(long)]
        follow: bool,
        /// Only events after this cursor (default: what the kernel still holds)
        #[arg(long, value_name = "CURSOR")]
        after: Option<u64>,
        /// One JSON record per line
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// Move a queued or sleeping process to another queue: interactive, background or
    /// maintenance. The moved process gets a new pid.
    #[cfg(feature = "harness")]
    Nice {
        /// Process id, or a unique prefix of it
        pid: String,
        /// The queue: interactive, background, maintenance or a named one
        #[arg(value_parser = parse_queue)]
        queue: adk_kernel::QueueClass,
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// The kernel's agenda: follow-ups proposed for later, and what became of them
    ///
    /// Example: `adk-rust agenda`, then `adk-rust agenda approve 3f2a` (spec 3.2 §8.1).
    #[cfg(feature = "harness")]
    Agenda {
        #[command(subcommand)]
        command: Option<AgendaCommand>,
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// Create a sleeping process that runs a task when a file changes, a cron schedule
    /// fires, or a webhook delivers
    ///
    /// Example: `adk-rust wake --file Cargo.lock -- "audit the new dependencies"`.
    #[cfg(feature = "harness")]
    Wake {
        /// Wake when a file matching this pattern changes, relative to the current
        /// directory: Cargo.lock, src/*.rs
        #[arg(long, value_name = "GLOB")]
        file: Option<String>,
        /// Wake each time this cron expression (UTC) fires, until killed
        #[arg(long, value_name = "EXPR")]
        cron: Option<String>,
        /// Wake when this webhook delivers to an ingress on the kernel's store
        #[arg(long, value_name = "NAME")]
        webhook: Option<String>,
        /// Run the task in this session (default: a new one)
        #[arg(long)]
        session: Option<String>,
        /// The task, after `--`
        #[arg(last = true, required = true, value_name = "TASK")]
        task: Vec<String>,
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// The kernel's drivers: MCP servers it supervises under the manifests you mounted
    #[cfg(feature = "harness")]
    Drivers {
        #[command(subcommand)]
        command: DriversCommands,
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// Phones, terminals and chat accounts that may reach your agents: pair one with a code,
    /// list them, remove one (spec 3.3 §10 K1, K2)
    ///
    /// Example: `adk-rust pair --name "Ana's phone"`, then enter the code on the phone;
    /// `adk-rust pair accept ABCD-2345` pairs the chat account that was sent that code.
    #[cfg(feature = "harness")]
    Pair {
        #[command(subcommand)]
        command: Option<crate::pair_cli::PairCommand>,
        #[command(flatten)]
        issue: crate::pair_cli::IssueArgs,
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// The kernel's self-observation: its content-free activity ledger, each partition's
    /// baselines, and the observations it offered as questions (spec 3.2 §7.10)
    ///
    /// Example: `adk-rust proc`, `adk-rust proc ledger --export ledger.jsonl`, then
    /// `cargo adk eval --ledger ledger.jsonl`.
    #[cfg(feature = "harness")]
    Proc {
        /// What to show
        #[arg(value_enum, default_value_t = ProcView::Observations)]
        view: ProcView,
        /// Only this partition
        #[arg(long)]
        partition: Option<String>,
        /// `ledger`: only rows of these kinds, comma-separated, such as tool_call,intent
        #[arg(long)]
        kind: Option<String>,
        /// `ledger`: only rows of this agent
        #[arg(long)]
        agent: Option<String>,
        /// `ledger`: only rows since this instant (RFC 3339) or day (YYYY-MM-DD); default
        /// the last 24 hours, or the whole ledger with --export
        #[arg(long)]
        since: Option<String>,
        /// At most this many rows or observations
        #[arg(long)]
        limit: Option<usize>,
        /// `ledger`: write every row as JSONL to this file (`-` for standard output), the
        /// format `cargo adk eval --ledger` reads
        #[arg(long, value_name = "FILE")]
        export: Option<std::path::PathBuf>,
        #[command(flatten)]
        kernel: KernelTarget,
    },

    /// Harness sessions kept in harness.db
    #[cfg(all(feature = "harness", feature = "persist"))]
    Sessions {
        #[command(subcommand)]
        command: SessionsCommands,
    },

    /// Standing rules: approvals and denials remembered across sessions
    #[cfg(all(feature = "harness", feature = "persist"))]
    Rules {
        #[command(subcommand)]
        command: RulesCommands,
        /// The rules of served replicas sharing this store, postgres://… or redis://…,
        /// instead of harness.db (spec 3.1 §13 Q2)
        #[arg(long, value_name = "URL", global = true)]
        store: Option<String>,
    },

    /// Long-term memory: the facts the harness learned about you and your projects
    #[cfg(all(feature = "harness", feature = "persist"))]
    Memory {
        #[command(subcommand)]
        command: MemoryCommands,
    },

    /// The encrypted store: which key harness.db is sealed under, and key rotation
    #[cfg(all(feature = "harness", feature = "persist", feature = "encrypted"))]
    Store {
        #[command(subcommand)]
        command: StoreCommands,

        /// Where the store key is: `keychain:<entry>`, `secret:<provider>/<name>` or `env:<VAR>`
        /// (default: `[store] key` in the user config)
        #[arg(long, global = true, value_name = "SOURCE")]
        store_key: Option<String>,
    },

    /// Calls waiting for a decision, here or on a served harness
    #[cfg(all(feature = "harness", feature = "persist"))]
    Approvals {
        #[command(subcommand)]
        command: ApprovalsCommands,

        /// A served harness to talk to instead of this machine's harness.db
        #[arg(long, global = true)]
        server: Option<String>,

        /// Bearer token for --server (default: $ADK_SERVE_TOKEN)
        #[arg(long, global = true)]
        token: Option<String>,
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

    /// Google Agent Registry registration and discovery
    #[cfg(feature = "vertex-agent-registry")]
    Registry {
        #[command(subcommand)]
        command: RegistryCommands,
    },

    /// Graph time-travel debugging commands
    Graph {
        #[command(subcommand)]
        command: GraphCommands,
    },

    /// Signed audit log tools: verify an export, generate a checkpoint key
    #[cfg(feature = "audit")]
    Audit {
        #[command(subcommand)]
        command: AuditCommands,
    },
}

/// Options for `adk-rust chat` (spec 3.1 §8.1).
///
/// With the `persist` feature, conversations are kept in `<config dir>/adk-rust/harness.db`.
#[derive(clap::Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatArgs {
    /// Continue the session with this id, creating it when it does not exist
    #[arg(long, conflicts_with_all = ["continue_session", "ephemeral"])]
    pub session: Option<String>,

    /// Continue the most recent session
    #[arg(long = "continue", conflicts_with = "ephemeral")]
    pub continue_session: bool,

    /// Keep the conversation in memory only; nothing is written to disk
    #[arg(long)]
    pub ephemeral: bool,
}

/// `adk-rust up` (spec 3.2 §8.1).
#[cfg(feature = "harness")]
#[derive(clap::Args, Debug, Clone)]
pub struct UpArgs {
    /// Install the kernel as a service that runs `adk-rust up` with these flags as you: a
    /// launchd agent (macOS), a systemd user unit (Linux) or a Windows service (an elevated
    /// shell and your password); it logs to `<home>/kernel.log` and restarts on failure
    #[arg(long, conflicts_with = "uninstall")]
    pub install: bool,
    /// Stop and remove the service --install set up for this kernel home
    #[arg(long)]
    pub uninstall: bool,
    /// With --install: print the service definition instead of installing it
    #[arg(long, requires = "install")]
    pub print: bool,
    /// Run as the Windows service NAME, which the service control manager starts
    #[cfg(windows)]
    #[arg(long, value_name = "NAME", hide = true)]
    pub windows_service: Option<String>,
    /// The kernel home: its socket and harness.db (default: `[kernel] home` in the user
    /// config, else the config directory's adk-rust)
    #[arg(long, value_name = "DIR")]
    pub home: Option<std::path::PathBuf>,
    /// Where the HTTP control plane and the served harness listen (default: `[kernel] bind`,
    /// else 127.0.0.1:8080); anything but loopback needs --auth-token
    #[arg(long, value_name = "HOST:PORT")]
    pub bind: Option<std::net::SocketAddr>,
    /// Bearer token callers must present (default: $ADK_SERVE_TOKEN); required beyond
    /// loopback
    #[arg(long)]
    pub auth_token: Option<String>,
    /// Keep every store, the kernel's included, in one backend replicas share:
    /// postgres://… or redis://… (default: harness.db)
    #[arg(long, value_name = "URL", conflicts_with = "ephemeral")]
    pub store: Option<String>,
    #[command(flatten)]
    pub harness: adk_cli::harness::HarnessArgs,
}

/// Which running kernel a control command reaches (spec 3.2 §7.11): the local socket of a
/// home, or an HTTP control plane.
#[cfg(feature = "harness")]
#[derive(clap::Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct KernelTarget {
    /// The home of the kernel to reach, on its socket (default: the config directory's
    /// adk-rust)
    #[arg(long, value_name = "DIR")]
    pub home: Option<std::path::PathBuf>,
    /// A kernel's HTTP control plane instead of the local socket, such as
    /// http://127.0.0.1:8080
    #[arg(long, value_name = "URL")]
    pub server: Option<String>,
    /// Bearer token for --server (default: $ADK_SERVE_TOKEN)
    #[arg(long)]
    pub token: Option<String>,
}

/// `adk-rust agenda` subcommands (spec 3.2 §8.1).
#[cfg(feature = "harness")]
#[derive(clap::Subcommand, Debug, Clone)]
pub enum AgendaCommand {
    /// The items, oldest first (the default)
    List {
        /// Only items of this partition
        #[arg(long)]
        partition: Option<String>,
        /// Only items in this state: proposed, approved, scheduled, running, done,
        /// rejected or expired
        #[arg(long)]
        state: Option<adk_kernel::AgendaStateKind>,
        /// At most this many items
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Approve a proposed item; it runs once admitted
    Approve {
        /// Item id, or a unique prefix of it
        id: String,
    },
    /// Reject a proposed or approved item
    Reject {
        /// Item id, or a unique prefix of it
        id: String,
        /// Why; recorded on the event and in the audit record
        #[arg(long)]
        reason: Option<String>,
    },
    /// Run an item at a time: approves a proposed item first
    Schedule {
        /// Item id, or a unique prefix of it
        id: String,
        /// When: an RFC 3339 time, HH:MM (local), or +30m, +2h, +1d
        #[arg(long, value_name = "TIME", conflicts_with = "cron")]
        at: Option<String>,
        /// At the next time this cron expression (UTC) fires
        #[arg(long, value_name = "EXPR")]
        cron: Option<String>,
    },
}

/// Parses a queue class: `interactive`, `background`, `maintenance` or a named queue.
#[cfg(feature = "harness")]
fn parse_queue(value: &str) -> Result<adk_kernel::QueueClass, String> {
    value.parse().map_err(|error: adk_kernel::KernelError| error.to_string())
}

/// `adk-rust drivers` (spec 3.2 §7.7, §8.1).
#[cfg(feature = "harness")]
#[derive(Subcommand, Clone, Debug, PartialEq, Eq)]
pub enum DriversCommands {
    /// The mounted drivers with their state, sandbox and stale tools
    List,
    /// Show what a manifest asks you to approve — the command line, the partitions, the
    /// effect classes — and mount it once you approve; the workspace's
    /// `.adk/drivers/<id>.toml`, if any, may only restrict it
    Mount {
        /// The manifest file
        file: std::path::PathBuf,
        /// Approve without asking
        #[arg(long)]
        yes: bool,
    },
    /// Stop a driver and forget its manifest and parked calls
    Unmount {
        /// Driver id
        id: String,
    },
    /// Restart a driver, minting its secrets again
    Restart {
        /// Driver id
        id: String,
    },
    /// Approve a driver's changed tool declarations; your standing rules follow them
    Reapprove {
        /// Driver id
        id: String,
    },
}

/// What `adk-rust proc` shows (spec 3.2 §7.10, §8.1).
#[cfg(feature = "harness")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProcView {
    /// The content-free activity ledger
    Ledger,
    /// Each partition's 28-day baselines and deviation tests
    Baselines,
    /// The observations offered as questions
    Observations,
}

/// Parses a run state as the run store names it: `waiting`, `running`, ….
#[cfg(feature = "harness")]
fn parse_run_state(value: &str) -> Result<adk_core::RunState, String> {
    serde_json::from_value(serde_json::Value::String(value.trim().to_ascii_lowercase())).map_err(
        |_| {
            format!(
                "unknown state '{value}'; use queued, running, waiting, completed, failed or \
                 cancelled"
            )
        },
    )
}

/// What `adk-rust serve` serves (spec 3.1 §8.2).
#[cfg(feature = "harness")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ServeAgent {
    /// The harness, in plan mode unless --mode says otherwise, with the approval inbox
    Harness,
    /// The plain chat agent, as before the harness
    Chat,
}

/// `adk-rust sessions` (spec 3.1 §8.1).
#[cfg(all(feature = "harness", feature = "persist"))]
#[derive(Subcommand, Clone, Debug, PartialEq, Eq)]
pub enum SessionsCommands {
    /// List your sessions, most recent first
    List,
    /// Show a session's conversation
    Show {
        /// Session id
        id: String,
    },
    /// Delete a session
    Rm {
        /// Session id
        id: String,
    },
}

/// `adk-rust rules` (spec 3.1 §8.1, §7.5).
#[cfg(all(feature = "harness", feature = "persist"))]
#[derive(Subcommand, Clone, Debug, PartialEq, Eq)]
pub enum RulesCommands {
    /// List your standing rules
    List,
    /// Remember a decision for a harness tool
    ///
    /// Example: `adk-rust rules add bash --prefix "cargo test"` allows every `cargo test …`
    /// command without asking; `--deny REASON` refuses instead.
    Add {
        /// The harness tool: bash, write_file, edit_file, or another tool's name with
        /// --operator
        tool: String,
        /// For bash: the command's leading words, such as "cargo test"
        #[arg(long)]
        prefix: Option<String>,
        /// For write_file and edit_file: the directory the path must be within
        #[arg(long)]
        path_within: Option<String>,
        /// Refuse matching calls with this reason instead of allowing them
        #[arg(long)]
        deny: Option<String>,
        /// Only in this session
        #[arg(long)]
        session: Option<String>,
        /// Expire after this many hours
        #[arg(long)]
        hours: Option<u32>,
        /// Pin the tool as the OS-sandboxed harness declares it
        #[arg(long)]
        sandbox: bool,
        /// Operator rule: any declaration from the tool's publisher (`--source`)
        #[arg(long)]
        operator: bool,
        /// With --operator: the tool's publisher, such as `mcp:github` (default `static`)
        #[arg(long, requires = "operator")]
        source: Option<String>,
    },
    /// Revoke a rule by its number in `rules list` or its id
    Rm {
        /// Rule number or id
        rule: String,
    },
}

/// `adk-rust memory` (spec 3.1 §7.15.2, §10 L2).
#[cfg(all(feature = "harness", feature = "persist"))]
#[derive(Subcommand, Clone, Debug, PartialEq, Eq)]
pub enum MemoryCommands {
    /// List the facts of this project and your preferences, known first
    List {
        /// The workspace whose project to list (default: the current directory)
        #[arg(long, value_name = "DIR")]
        dir: Option<std::path::PathBuf>,
        /// Every project, and superseded and forgotten facts too
        #[arg(long)]
        all: bool,
        /// Only facts of this source: consolidation (written by the kernel's consolidation
        /// tick, with its ticks), user_stated, user_confirmed, agent_inferred, tool_observed
        /// or imported
        #[arg(long, value_parser = [
            "consolidation", "user_stated", "user_confirmed", "agent_inferred", "tool_observed", "imported",
        ])]
        source: Option<String>,
    },
    /// Revert a consolidation tick's status changes: what it superseded or forgot is assumed
    /// again, and what it wrote is forgotten (spec 3.2 §7.5)
    Undo {
        /// Tick id, or a unique prefix of it, as `memory list --source consolidation` shows
        tick: String,
    },
    /// Show one fact with its provenance
    Show {
        /// Fact id, or a unique prefix of it
        id: String,
    },
    /// Forget a fact: it is never recalled again
    Forget {
        /// Fact id, or a unique prefix of it
        id: String,
    },
    /// Confirm an assumed fact: it becomes known
    Accept {
        /// Fact id, or a unique prefix of it
        id: String,
    },
    /// Write every fact, with provenance and history, as JSON lines
    Export {
        /// File to write (default: standard output)
        #[arg(long, value_name = "FILE")]
        out: Option<std::path::PathBuf>,
    },
    /// Read facts from JSON lines `memory export` wrote; they arrive assumed
    Import {
        /// File to read
        file: std::path::PathBuf,
    },
}

/// `adk-rust store` (spec 3.1 §7.12, §8.1).
#[cfg(all(feature = "harness", feature = "persist", feature = "encrypted"))]
#[derive(Subcommand, Clone, Debug, PartialEq, Eq)]
pub enum StoreCommands {
    /// Report the key source, the key harness.db is sealed under, and which stores are wrapped
    Status,
    /// Re-seal every store under a new key and retire the current one; seals rows written
    /// before a key was configured
    RotateKey {
        /// The new key, base64 of 32 bytes (default: a generated one)
        #[arg(long, value_name = "BASE64")]
        key: Option<String>,
    },
}

/// `adk-rust approvals` (spec 3.1 §8.1, §8.2).
#[cfg(all(feature = "harness", feature = "persist"))]
#[derive(Subcommand, Clone, Debug, PartialEq, Eq)]
pub enum ApprovalsCommands {
    /// List calls waiting for a decision
    List,
    /// Approve a call once
    Approve {
        /// The id `approvals list` shows
        id: String,
    },
    /// Deny a call
    Deny {
        /// The id `approvals list` shows
        id: String,
        /// Reported to the model
        #[arg(long)]
        reason: Option<String>,
    },
}

#[cfg(feature = "audit")]
#[derive(Subcommand, Clone)]
pub enum AuditCommands {
    /// Verify an exported audit log against its signed checkpoints.
    ///
    /// Exits 0 when the log verifies and 1 when it does not.
    /// Example: `adk-rust audit verify audit.jsonl --key "$(cat audit.vkey)"`
    Verify {
        /// Export file (JSON lines of entries and checkpoints)
        export: String,

        /// Trusted verifier key, `<name>+<hash>+<base64>`; repeat for several keys
        #[arg(long = "key", required_unless_present = "key_file")]
        keys: Vec<String>,

        /// File with one trusted verifier key per line
        #[arg(long)]
        key_file: Option<String>,

        /// Extra checkpoints, such as a `FileCheckpointAnchor` file; repeatable
        #[arg(long = "checkpoints")]
        checkpoints: Vec<String>,

        /// Print the report as JSON
        #[arg(long)]
        json: bool,
    },

    /// Export a log from a SQLite or PostgreSQL audit store as JSON lines.
    #[cfg(feature = "audit-sql")]
    Export {
        /// Database URL, `sqlite://...` or `postgres://...`
        #[arg(long)]
        database_url: String,

        /// Log id to export
        #[arg(long)]
        log_id: String,

        /// Output file; standard output when omitted
        #[arg(long)]
        out: Option<String>,
    },

    /// Generate an Ed25519 checkpoint key pair in signed-note format.
    ///
    /// Writes `<out>.key` (secret signer key) and `<out>.vkey` (verifier key).
    Keygen {
        /// Key name that appears in checkpoint signatures, e.g. `audit.example.com`
        name: String,

        /// Output path prefix
        #[arg(long, default_value = "audit")]
        out: String,
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

    /// Print the recorded state at each checkpointed step in a range (reads only)
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
    /// Search the Vertex AI Skill Registry (read-only, v1beta1 Preview)
    #[cfg(feature = "vertex-skill-registry")]
    Search {
        /// Query text for semantic search over skill names and descriptions
        query: String,
        /// Maximum results (server default 10, max 100)
        #[arg(long)]
        top_k: Option<u32>,
        /// Google Cloud project ID (defaults to $GOOGLE_CLOUD_PROJECT)
        #[arg(long)]
        project: Option<String>,
        /// GCP region: us-central1, europe-west4, or us-east5 (defaults to
        /// $GOOGLE_CLOUD_LOCATION)
        #[arg(long)]
        location: Option<String>,
        /// Override the API origin. Advanced: for private endpoints and
        /// tests; must be a bare HTTPS (or loopback HTTP) origin.
        #[arg(long)]
        endpoint: Option<String>,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Pull a skill package from the Vertex AI Skill Registry into a local
    /// skills directory (read-only against the registry; never publishes)
    #[cfg(feature = "vertex-skill-registry")]
    Pull {
        /// Skill ID or full projects/*/locations/*/skills/* resource name
        name: String,
        /// Revision ID to pin (default: the latest revision)
        #[arg(long)]
        revision: Option<String>,
        /// Directory to materialize the skill under (one subdirectory per
        /// skill)
        #[arg(long, default_value = ".skills")]
        dir: String,
        /// Google Cloud project ID (defaults to $GOOGLE_CLOUD_PROJECT)
        #[arg(long)]
        project: Option<String>,
        /// GCP region: us-central1, europe-west4, or us-east5 (defaults to
        /// $GOOGLE_CLOUD_LOCATION)
        #[arg(long)]
        location: Option<String>,
        /// Override the API origin. Advanced: for private endpoints and
        /// tests; must be a bare HTTPS (or loopback HTTP) origin.
        #[arg(long)]
        endpoint: Option<String>,
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
    /// Deploy a container image as a Gemini Enterprise Agent Platform engine
    #[cfg(feature = "gcp-deploy")]
    AgentEngine(AgentEngineArgs),
}

/// Arguments for `adk-rust deploy agent-engine`.
///
/// A named struct rather than inline variant fields so later releases can
/// extend the surface (e.g. Agent Registry self-registration) without
/// re-plumbing the dispatch.
#[cfg(feature = "gcp-deploy")]
#[derive(clap::Args, Clone, Debug)]
pub struct AgentEngineArgs {
    /// Artifact Registry image URI of the agent container
    /// (e.g. us-central1-docker.pkg.dev/PROJECT/REPO/agent:latest)
    #[arg(long)]
    pub image_uri: String,
    /// GCP project ID
    #[arg(long)]
    pub project: String,
    /// GCP location (e.g. us-central1)
    #[arg(long)]
    pub location: String,
    /// Service account the engine runs as (defaults to the platform's
    /// Reasoning Engine service agent)
    #[arg(long)]
    pub service_account: Option<String>,
    /// Customer-managed encryption key resource name (CMEK)
    #[arg(long)]
    pub kms_key: Option<String>,
    /// Engine display name (defaults to the image name)
    #[arg(long)]
    pub display_name: Option<String>,
    /// Override the API origin. Advanced: for private endpoints and tests;
    /// loopback HTTP is allowed, anything else must be HTTPS.
    #[arg(long, hide = true)]
    pub endpoint: Option<String>,
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

/// `adk-rust registry` — register self-hosted agents, external MCP servers,
/// and bare endpoints in the Google Agent Registry, and search it.
///
/// Registrations are idempotent: re-running a `register-*` command patches
/// the existing service instead of duplicating it. Manual registrations are
/// **not lifecycle-synced** — Agent Runtime deployments register themselves
/// automatically, so never register an engine you deployed with
/// `deploy agent-engine`; doing so would duplicate it under a different URN
/// namespace.
#[cfg(feature = "vertex-agent-registry")]
#[derive(Subcommand, Clone)]
pub enum RegistryCommands {
    /// Register (or idempotently update) a self-hosted agent
    RegisterAgent(RegistryRegisterAgentArgs),
    /// Register (or idempotently update) an external MCP server
    RegisterMcp(RegistryRegisterMcpArgs),
    /// Register (or idempotently update) a bare endpoint
    RegisterEndpoint(RegistryRegisterEndpointArgs),
    /// Search registered agents or MCP servers
    Search(RegistrySearchArgs),
}

/// Project/location scope shared by every `registry` subcommand.
#[cfg(feature = "vertex-agent-registry")]
#[derive(clap::Args, Clone, Debug)]
pub struct RegistryScopeArgs {
    /// GCP project ID (falls back to GOOGLE_CLOUD_PROJECT). Cross-project
    /// governance registration is supported by pointing this at the
    /// governed project.
    #[arg(long)]
    pub project: Option<String>,
    /// Registry location, e.g. `global` or a region (falls back to
    /// GOOGLE_CLOUD_LOCATION). The `us`/`eu` multi-regions are unsupported.
    #[arg(long)]
    pub location: Option<String>,
    /// Override the API origin. Advanced: for private endpoints and tests;
    /// loopback HTTP is allowed, anything else must be HTTPS.
    #[arg(long, hide = true)]
    pub endpoint: Option<String>,
}

/// Arguments for `adk-rust registry register-agent`.
#[cfg(feature = "vertex-agent-registry")]
#[derive(clap::Args, Clone, Debug)]
pub struct RegistryRegisterAgentArgs {
    /// Service ID to create or update (4-63 characters)
    #[arg(long)]
    pub service_id: String,
    /// Path to an A2A agent card JSON file (A2A_AGENT_CARD registration;
    /// the registry derives the interfaces from the card)
    #[arg(long, conflicts_with_all = ["url", "protocol"], required_unless_present = "url")]
    pub card: Option<String>,
    /// Agent endpoint URL (NO_SPEC registration)
    #[arg(long, required_unless_present = "card")]
    pub url: Option<String>,
    /// Protocol binding served at --url
    #[arg(long, value_enum, requires = "url")]
    pub protocol: Option<RegistryProtocol>,
    /// Display name (defaults to the service ID)
    #[arg(long)]
    pub display_name: Option<String>,
    /// Description (omit to leave an existing description unchanged)
    #[arg(long)]
    pub description: Option<String>,
    #[command(flatten)]
    pub scope: RegistryScopeArgs,
}

/// Arguments for `adk-rust registry register-mcp`.
#[cfg(feature = "vertex-agent-registry")]
#[derive(clap::Args, Clone, Debug)]
pub struct RegistryRegisterMcpArgs {
    /// Service ID to create or update (4-63 characters)
    #[arg(long)]
    pub service_id: String,
    /// Path to the server's tools/list result JSON (TOOL_SPEC registration;
    /// the file is sent as-is, no server introspection is performed)
    #[arg(long)]
    pub tool_spec: String,
    /// MCP endpoint URL
    #[arg(long)]
    pub url: String,
    /// Protocol binding served at --url
    #[arg(long, value_enum)]
    pub protocol: Option<RegistryProtocol>,
    /// Display name (defaults to the service ID)
    #[arg(long)]
    pub display_name: Option<String>,
    /// Description (omit to leave an existing description unchanged)
    #[arg(long)]
    pub description: Option<String>,
    #[command(flatten)]
    pub scope: RegistryScopeArgs,
}

/// Arguments for `adk-rust registry register-endpoint`.
#[cfg(feature = "vertex-agent-registry")]
#[derive(clap::Args, Clone, Debug)]
pub struct RegistryRegisterEndpointArgs {
    /// Service ID to create or update (4-63 characters)
    #[arg(long)]
    pub service_id: String,
    /// Endpoint URL
    #[arg(long)]
    pub url: String,
    /// Protocol binding served at --url
    #[arg(long, value_enum)]
    pub protocol: Option<RegistryProtocol>,
    /// Display name (defaults to the service ID)
    #[arg(long)]
    pub display_name: Option<String>,
    /// Description (omit to leave an existing description unchanged)
    #[arg(long)]
    pub description: Option<String>,
    #[command(flatten)]
    pub scope: RegistryScopeArgs,
}

/// Arguments for `adk-rust registry search`.
#[cfg(feature = "vertex-agent-registry")]
#[derive(clap::Args, Clone, Debug)]
pub struct RegistrySearchArgs {
    /// Search expression (bare words match word-contains, field="value"
    /// matches exactly, NOT/AND/OR and parentheses combine, `*` suffix
    /// matches prefixes)
    pub query: String,
    /// Which registry component to search
    #[arg(long = "type", value_enum, default_value = "agent")]
    pub component_type: RegistrySearchType,
    #[command(flatten)]
    pub scope: RegistryScopeArgs,
}

/// Protocol bindings accepted by `--protocol`.
#[cfg(feature = "vertex-agent-registry")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RegistryProtocol {
    /// JSON-RPC over HTTP
    Jsonrpc,
    /// gRPC
    Grpc,
    /// REST-style JSON over HTTP
    HttpJson,
}

/// Searchable registry components (endpoints have no search).
#[cfg(feature = "vertex-agent-registry")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RegistrySearchType {
    /// Search agents
    Agent,
    /// Search MCP servers
    McpServer,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|error| panic!("{args:?}: {error}"))
    }

    fn chat_args(args: &[&str]) -> ChatArgs {
        match parse(args).command {
            Some(Commands::Chat(chat)) => chat,
            _ => panic!("{args:?} is not a chat command"),
        }
    }

    #[test]
    fn chat_selects_how_the_session_is_kept() {
        assert_eq!(chat_args(&["adk-rust", "chat"]), ChatArgs::default());
        assert_eq!(
            chat_args(&["adk-rust", "chat", "--session", "release-notes"]),
            ChatArgs { session: Some("release-notes".into()), ..ChatArgs::default() }
        );
        assert_eq!(
            chat_args(&["adk-rust", "chat", "--continue"]),
            ChatArgs { continue_session: true, ..ChatArgs::default() }
        );
        assert_eq!(
            chat_args(&["adk-rust", "chat", "--ephemeral"]),
            ChatArgs { ephemeral: true, ..ChatArgs::default() }
        );
        assert!(parse(&["adk-rust"]).command.is_none());
    }

    #[test]
    fn session_flags_exclude_each_other() {
        for args in [
            &["adk-rust", "chat", "--session", "a", "--continue"][..],
            &["adk-rust", "chat", "--session", "a", "--ephemeral"],
            &["adk-rust", "chat", "--continue", "--ephemeral"],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?} should not parse");
        }
    }

    #[test]
    fn instruction_reaches_the_coding_commands() {
        for args in [
            &["adk-rust", "code", "fix it", "--instruction", "use tabs"][..],
            &["adk-rust", "goal", "green", "--until", "true", "--instruction", "use tabs"],
            &["adk-rust", "--instruction", "use tabs", "ultracode", "fix it"],
        ] {
            assert_eq!(parse(args).options.instruction.as_deref(), Some("use tabs"), "{args:?}");
        }
    }

    #[test]
    fn compiled_providers_are_exactly_the_enabled_features() {
        let compiled = compiled_providers();
        for (provider, enabled) in [
            (Provider::Model(ModelProvider::Gemini), cfg!(feature = "gemini")),
            (Provider::Model(ModelProvider::Openai), cfg!(feature = "openai")),
            (Provider::Model(ModelProvider::Anthropic), cfg!(feature = "anthropic")),
            (Provider::Model(ModelProvider::Deepseek), cfg!(feature = "deepseek")),
            (Provider::Model(ModelProvider::Groq), cfg!(feature = "groq")),
            (Provider::Model(ModelProvider::Ollama), cfg!(feature = "ollama")),
            (Provider::MistralRs, cfg!(feature = "mistralrs")),
        ] {
            assert_eq!(compiled.contains(&provider), enabled, "{provider}");
        }
        let order: Vec<_> =
            Provider::all().iter().copied().filter(|p| compiled.contains(p)).collect();
        assert_eq!(compiled, order, "menu order follows ModelProvider::all");
    }

    #[cfg(feature = "harness")]
    #[test]
    fn ultracode_flags_parse() {
        use adk_cli::harness::ultracode::{ArchitectureChoice, RouterMode};

        let parsed = parse(&[
            "adk-rust",
            "ultracode",
            "fix it",
            "--architecture",
            "blackboard",
            "--router",
            "heuristic",
            "--max-model-calls",
            "300",
            "--max-rounds",
            "3",
            "--receipt",
            "out/receipt.json",
        ]);
        let Some(Commands::Ultracode {
            task,
            max_rounds,
            architecture,
            router,
            max_model_calls,
            receipt,
            ..
        }) = parsed.command
        else {
            panic!("not ultracode");
        };
        assert_eq!(task, "fix it");
        assert_eq!(
            (max_rounds, architecture, router, max_model_calls),
            (Some(3), Some(ArchitectureChoice::Blackboard), Some(RouterMode::Heuristic), Some(300))
        );
        assert_eq!(receipt.as_deref(), Some(std::path::Path::new("out/receipt.json")));
        let Some(Commands::Ultracode { max_rounds, architecture, .. }) =
            parse(&["adk-rust", "ultracode", "fix it"]).command
        else {
            panic!("not ultracode");
        };
        assert_eq!((max_rounds, architecture), (None, None));
        assert!(
            Cli::try_parse_from(["adk-rust", "ultracode", "x", "--architecture", "council"])
                .is_err()
        );
    }

    #[cfg(feature = "scripted-model")]
    #[test]
    fn test_hooks_parse_but_stay_hidden() {
        let cli = parse(&["adk-rust", "chat", "--model-script", "[]", "--approval-fixture"]);
        assert_eq!(cli.options.model_script.as_deref(), Some("[]"));
        assert!(cli.options.approval_fixture);
        let help = <Cli as clap::CommandFactory>::command().render_long_help().to_string();
        assert!(!help.contains("model-script") && !help.contains("approval-fixture"), "{help}");
    }

    #[cfg(all(feature = "harness", feature = "persist"))]
    #[test]
    fn memory_lists_consolidation_ticks_undoes_one_and_up_turns_the_tick_off() {
        let memory = |args: &[&str]| match parse(args).command {
            Some(Commands::Memory { command }) => command,
            _ => panic!("{args:?} is not a memory command"),
        };
        assert_eq!(
            memory(&["adk-rust", "memory", "list", "--source", "consolidation"]),
            MemoryCommands::List { dir: None, all: false, source: Some("consolidation".into()) }
        );
        assert_eq!(
            memory(&["adk-rust", "memory", "undo", "01J9"]),
            MemoryCommands::Undo { tick: "01J9".into() }
        );
        assert!(
            Cli::try_parse_from(["adk-rust", "memory", "list", "--source", "dream"]).is_err(),
            "an unknown source should not parse"
        );
        let Some(Commands::Up(up)) = parse(&["adk-rust", "up", "--no-consolidation"]).command
        else {
            panic!("up should parse");
        };
        assert!(up.harness.no_consolidation && !up.harness.no_memory);
    }
}
