fn main() {
    println!("Graph Agent Test Examples");
    println!("========================");
    println!();
    println!("Available examples:");
    println!("  cargo run --bin parallel_processing   # Translation + summarization in parallel");
    println!("  cargo run --bin conditional_routing   # Sentiment-based routing");
    println!("  cargo run --bin react_pattern         # Iterative reasoning with tools");
    println!("  cargo run --bin supervisor_routing    # Route tasks to specialists");
    println!("  cargo run --bin human_in_loop         # Risk-based approval workflow");
    println!("  cargo run --bin checkpointing         # State persistence and time travel");
    println!();
    println!("Set GOOGLE_API_KEY environment variable before running examples.");
    println!("All examples include safety limits to prevent infinite loops.");
}
