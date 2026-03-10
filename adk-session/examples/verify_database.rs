#[cfg(feature = "database")]
use adk_session::*;

#[cfg(feature = "database")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔍 Verifying existing database...\n");

    let service = SqliteSessionService::new("sqlite:test_adk.db").await?;

    // List all sessions
    let sessions = service
        .list(ListRequest {
            app_name: "test_app".to_string(),
            user_id: "user1".to_string(),
            limit: None,
            offset: None,
        })
        .await?;

    println!("📊 Found {} session(s) in database:", sessions.len());
    for session in sessions {
        println!("   - Session ID: [redacted]");
        println!("     App: [redacted]");
        println!("     User: [redacted]");
        println!("     Events: {}", session.events().len());
        println!("     State keys: {}", session.state().all().len());
    }

    println!("\n✅ Database verification complete!");

    Ok(())
}

#[cfg(not(feature = "database"))]
fn main() {
    println!("This example requires the 'database' feature.");
    println!("Run with: cargo run --example verify_database --features database");
}
