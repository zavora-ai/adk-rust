use livekit::prelude::{Room, RoomOptions};
use livekit_api::access_token::{AccessToken, VideoGrants};
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls default crypto provider");
    tracing_subscriber::fmt::init();

    // Initialize the crypto provider for rustls 0.23
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let url = env::var("LIVEKIT_URL").expect("LIVEKIT_URL required");
    let key = env::var("LIVEKIT_API_KEY").expect("LIVEKIT_API_KEY required");
    let secret = env::var("LIVEKIT_API_SECRET").expect("LIVEKIT_API_SECRET required");

    println!("URL: {}", url);
    println!("API_KEY: {}", key);

    // 1. Generate Token
    let mut token_builder = AccessToken::with_api_key(&key, &secret);
    token_builder = token_builder.with_identity("debug-agent-01");
    token_builder = token_builder.with_name("Debug Agent");

    let mut grants = VideoGrants::default();
    grants.room_join = true;
    grants.room_create = true;
    grants.room_list = true;
    grants.room = "my-room".to_string(); // Matching the example
    token_builder = token_builder.with_grants(grants);

    let token = token_builder.to_jwt()?;
    println!("Generated Token length: {}", token.len());
    println!("Token start: {}...", &token[..10]);
    println!("Token end: ...{}", &token[token.len() - 10..]);

    // 2. Attempt Connection
    println!("Connecting to LiveKit room...");
    let options = RoomOptions::default();

    match Room::connect(&url, &token, options).await {
        Ok((room, _events)) => {
            println!("SUCCESS: Connected to room: {}", room.name());
            room.close().await?;
        }
        Err(e) => {
            println!("FAILURE: Failed to connect: {:?}", e);
        }
    }

    Ok(())
}
