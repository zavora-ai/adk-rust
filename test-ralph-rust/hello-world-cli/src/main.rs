#![allow(unused_imports)]

mod cli;

use crate::cli::Cli;
use clap::Parser;
use std::process::exit;

fn main() {
    let result = Cli::try_parse();
    match result {
        Ok(_cli) => {
            // If parsing is successful, proceed with normal execution (e.g., printing Hello, world!)
            println!("Hello, world!");
        }
        Err(e) => {
            // If parsing fails, clap already prints the error message to stderr.
            // We just need to ensure a non-zero exit code.
            eprintln!("{}", e);
            exit(1);
        }
    }
}
