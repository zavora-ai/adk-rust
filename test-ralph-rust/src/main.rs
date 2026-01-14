use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
    println!("Hello, world!");
}
