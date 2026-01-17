// src/cli.rs

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(author, version, about = "A simple CLI calculator", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Adds two numbers
    Add {
        #[arg(help = "The first number")]
        num1: f64,
        #[arg(help = "The second number")]
        num2: f64,
    },
    /// Subtracts two numbers
    Subtract {
        #[arg(help = "The first number")]
        num1: f64,
        #[arg(help = "The second number")]
        num2: f64,
    },
    /// Multiplies two numbers
    Multiply {
        #[arg(help = "The first number")]
        num1: f64,
        #[arg(help = "The second number")]
        num2: f64,
    },
    /// Divides two numbers
    Divide {
        #[arg(help = "The first number")]
        num1: f64,
        #[arg(help = "The second number")]
        num2: f64,
    },
}
