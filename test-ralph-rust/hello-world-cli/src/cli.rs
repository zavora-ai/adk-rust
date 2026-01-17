#![allow(unused_imports)]

use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    // No arguments yet, but clap is initialized
}