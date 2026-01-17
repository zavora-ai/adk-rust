// main.rs

mod cli;

use clap::Parser;
use cli::{Cli, Commands};

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Add { num1, num2 } => {
            println!("Add: {} + {} = ?", num1, num2);
        }
        Commands::Subtract { num1, num2 } => {
            println!("Subtract: {} - {} = ?", num1, num2);
        }
        Commands::Multiply { num1, num2 } => {
            println!("Multiply: {} * {} = ?", num1, num2);
        }
        Commands::Divide { num1, num2 } => {
            println!("Divide: {} / {} = ?", num1, num2);
        }
    }
}
