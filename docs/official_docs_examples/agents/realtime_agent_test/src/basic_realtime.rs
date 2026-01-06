//! Basic Realtime Session Example
//! 
//! Demonstrates the low-level session API for text-based realtime interactions.

use adk_realtime::{
    openai::OpenAIRealtimeModel,
    RealtimeConfig, RealtimeModel, ServerEvent,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("OPENAI_API_KEY")?;

    println!("🎙️  Basic Realtime Session Example");
    println!("This demonstrates text-based realtime interactions\n");

    // Create the realtime model
    let model = OpenAIRealtimeModel::new(&api_key, "gpt-4o-realtime-preview-2024-12-17");

    // Configure the session
    let config = RealtimeConfig::default()
        .with_instruction("You are a helpful assistant. Be concise and friendly.")
        .with_voice("alloy")
        .with_modalities(vec!["text".to_string()]);  // Text-only for this example

    println!("📡 Connecting to OpenAI Realtime API...");
    let session = model.connect(config).await?;
    println!("✅ Connected!\n");

    // Send a text message
    let message = "Hello! What's 2 + 2?";
    println!("👤 User: {}", message);
    session.send_text(message).await?;
    session.create_response().await?;

    // Process events
    print!("🤖 Assistant: ");
    while let Some(event) = session.next_event().await {
        match event? {
            ServerEvent::TextDelta { delta, .. } => {
                print!("{}", delta);
            }
            ServerEvent::ResponseDone { .. } => {
                println!("\n");
                break;
            }
            ServerEvent::Error { error, .. } => {
                println!("\n❌ Error: {:?}", error);
                break;
            }
            _ => {}
        }
    }

    // Send another message
    let message2 = "Now multiply that by 10";
    println!("👤 User: {}", message2);
    session.send_text(message2).await?;
    session.create_response().await?;

    print!("🤖 Assistant: ");
    while let Some(event) = session.next_event().await {
        match event? {
            ServerEvent::TextDelta { delta, .. } => {
                print!("{}", delta);
            }
            ServerEvent::ResponseDone { .. } => {
                println!("\n");
                break;
            }
            _ => {}
        }
    }

    println!("✅ Session complete!");
    Ok(())
}
