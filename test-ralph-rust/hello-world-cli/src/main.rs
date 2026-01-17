#![allow(unused_imports)]

mod cli;
mod message_handler;

use crate::cli::Cli;
use crate::message_handler::print_error;
use crate::message_handler::print_hello_world;
use clap::Parser;
use std::process::exit;

fn main() {
    let result = Cli::try_parse();
    match result {
        Ok(_cli) => {
            print_hello_world();
        }
        Err(e) => {
            print_error(&e.to_string());
            exit(1);
        }
    }
}
