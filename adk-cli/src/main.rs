mod cli;
mod config;
mod console;
mod serve;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    
    match cli.command {
        Commands::Console { app_name: _, user_id: _ } => {
            println!("Console mode requires an agent implementation.");
            println!("Use the quickstart example instead:");
            println!("  cargo run --example quickstart");
            Ok(())
        }
        Commands::Serve { port: _ } => {
            println!("Serve mode requires an agent loader.");
            println!("Create a custom binary that uses adk_cli::serve::run_serve()");
            println!("See examples for usage patterns.");
            Ok(())
        }
    }
}
