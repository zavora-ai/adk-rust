//! Generate the ADK-Rust podcast episode.
//!
//! ```bash
//! export GOOGLE_API_KEY=your-key-here
//! cargo run --manifest-path examples/gemini_audio/Cargo.toml --bin generate-podcast
//! ```

mod podcast;

fn load_dotenv() {
    let mut dir = std::env::current_dir().ok();
    while let Some(d) = dir {
        let path = d.join(".env");
        if path.is_file() {
            let _ = dotenvy::from_path(path);
            return;
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();

    println!("🎧 ADK-Rust Podcast Generator");
    println!("==============================\n");

    // Generate to the repo's docs directory
    let output = std::path::PathBuf::from("docs/podcast/adk-rust-episode-1.wav");
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }

    podcast::generate_podcast(&output).await?;

    println!("\n🎧 Podcast ready! Listen at: {}", output.display());
    Ok(())
}
