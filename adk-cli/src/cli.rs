use adk_core::types::UserId;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "adk")]
#[command(about = "Agent Development Kit CLI", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run agent in interactive console mode
    Console {
        /// Agent application name
        #[arg(short, long, default_value = "console_app")]
        app_name: String,

        /// User ID for session
        #[arg(short, long, default_value = "console_user")]
        user_id: UserId,
    },

    /// Start web server
    Serve {
        /// Server port
        #[arg(short, long, default_value = "8080")]
        port: u16,
    },
    /// Skills tooling (list/validate/match)
    Skills {
        #[command(subcommand)]
        command: SkillsCommands,
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
