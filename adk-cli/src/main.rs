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
            println!("Console mode not yet fully implemented");
            println!("Need to provide agent implementation");
            println!("See examples/quickstart.rs for usage");
            Ok(())
        }
        Commands::Serve { port: _ } => {
            println!("Serve mode not yet fully implemented");
            println!("Need to provide agent loader");
            println!("See examples/quickstart.rs for usage");
            Ok(())
        }
    }
}
