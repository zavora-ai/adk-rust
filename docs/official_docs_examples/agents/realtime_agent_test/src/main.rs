fn main() {
    println!("Realtime Agent Test Examples");
    println!("============================");
    println!();
    println!("Available examples:");
    println!("  cargo run --bin basic_realtime      # Simple text-based session");
    println!("  cargo run --bin realtime_with_tools # Tool calling during session");
    println!("  cargo run --bin realtime_vad        # Voice Activity Detection");
    println!("  cargo run --bin realtime_handoff    # Multi-agent handoffs");
    println!();
    println!("Set OPENAI_API_KEY environment variable before running examples.");
    println!("Note: These examples use text mode for easier testing.");
}
