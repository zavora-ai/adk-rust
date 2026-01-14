#![allow(unused_imports)]

mod cli;

use crate::cli::Cli;
use clap::Parser;

fn main() {
    let _cli = Cli::parse();
    println!("Hello, world!");
}
