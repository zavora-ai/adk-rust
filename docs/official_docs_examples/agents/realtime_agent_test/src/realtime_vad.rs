//! Realtime VAD Configuration Example
//! 
//! Demonstrates Voice Activity Detection settings for natural conversations.

use adk_realtime::{
    openai::OpenAIRealtimeModel,
    RealtimeConfig, RealtimeModel, ServerEvent, VadConfig, VadMode,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("OPENAI_API_KEY")?;

    println!("🎤 Realtime VAD Configuration Example");
    println!("This demonstrates Voice Activity Detection settings\n");

    // Create the realtime model
    let model = OpenAIRealtimeModel::new(&api_key, "gpt-4o-realtime-preview-2024-12-17");

    // Configure VAD for natural conversation
    let vad = VadConfig {
        mode: VadMode::ServerVad,
        threshold: Some(0.5),           // Speech detection sensitivity (0.0-1.0)
        prefix_padding_ms: Some(300),   // Audio to include before speech
        silence_duration_ms: Some(500), // Silence before ending turn
        interrupt_response: Some(true), // Allow interrupting assistant
        eagerness: None,                // For SemanticVad mode only
    };

    println!("📋 VAD Configuration:");
    println!("   Mode: ServerVad");
    println!("   Threshold: 0.5");
    println!("   Prefix padding: 300ms");
    println!("   Silence duration: 500ms");
    println!("   Interrupt enabled: true\n");

    // Configure the session with VAD
    let config = RealtimeConfig::default()
        .with_instruction("You are a helpful voice assistant. Keep responses brief and natural.")
        .with_voice("alloy")
        .with_modalities(vec!["text".to_string(), "audio".to_string()])
        .with_vad(vad);

    println!("📡 Connecting with VAD enabled...");
    let session = model.connect(config).await?;
    println!("✅ Connected!\n");

    // Simulate a conversation (text mode for testing)
    let message = "Tell me a short joke";
    println!("👤 User: {}", message);
    session.send_text(message).await?;
    session.create_response().await?;

    // Process events including VAD events
    print!("🤖 Assistant: ");
    while let Some(event) = session.next_event().await {
        match event? {
            ServerEvent::TextDelta { delta, .. } => {
                print!("{}", delta);
            }
            ServerEvent::SpeechStarted { .. } => {
                println!("\n🎤 [VAD: Speech started]");
            }
            ServerEvent::SpeechStopped { .. } => {
                println!("🔇 [VAD: Speech stopped]");
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

    println!("✅ VAD demonstration complete!");
    println!("\n📝 Notes:");
    println!("   - In a real voice app, VAD detects when you start/stop speaking");
    println!("   - SpeechStarted fires when voice is detected");
    println!("   - SpeechStopped fires after silence_duration_ms of silence");
    println!("   - interrupt_response allows you to cut off the assistant mid-sentence");

    Ok(())
}
