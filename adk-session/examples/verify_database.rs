use adk_session::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔍 Verifying existing database...\n");
    
    let service = DatabaseSessionService::new("sqlite:test_adk.db").await?;
    
    // List all sessions
    let sessions = service.list(ListRequest {
        app_name: "test_app".to_string(),
        user_id: "user1".to_string(),
    }).await?;
    
    println!("📊 Found {} session(s) in database:", sessions.len());
    for session in sessions {
        println!("   - Session ID: {}", session.id());
        println!("     App: {}", session.app_name());
        println!("     User: {}", session.user_id());
        println!("     Events: {}", session.events().len());
        println!("     State keys: {}", session.state().all().len());
    }
    
    println!("\n✅ Database verification complete!");
    
    Ok(())
}
