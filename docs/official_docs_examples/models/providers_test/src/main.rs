fn main() {
    println!("Model Providers Test Examples");
    println!("==============================");
    println!();
    println!("Available examples:");
    println!("  cargo run --bin gemini_example     # Google Gemini (default)");
    println!("  cargo run --bin openai_example     # OpenAI GPT-4o");
    println!("  cargo run --bin anthropic_example  # Anthropic Claude");
    println!("  cargo run --bin deepseek_example   # DeepSeek");
    println!("  cargo run --bin groq_example       # Groq (ultra-fast)");
    println!("  cargo run --bin ollama_example     # Ollama (local)");
    println!();
    println!("Set the appropriate API key before running:");
    println!("  GOOGLE_API_KEY, OPENAI_API_KEY, ANTHROPIC_API_KEY,");
    println!("  DEEPSEEK_API_KEY, GROQ_API_KEY");
    println!();
    println!("Ollama requires: ollama serve && ollama pull llama3.2");
}
