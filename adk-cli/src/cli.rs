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
        user_id: String,
    },

    /// Start web server
    Serve {
        /// Server port
        #[arg(short, long, default_value = "8080")]
        port: u16,
    },
}
