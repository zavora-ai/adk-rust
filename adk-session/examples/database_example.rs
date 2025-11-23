use adk_session::*;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = "test_adk.db";
    println!("🔧 Creating SQLite database at {}", db_path);
    
    let service = DatabaseSessionService::new(&format!("sqlite:{}", db_path)).await?;
    service.migrate().await?;
    
    println!("✅ Database created and migrated");
    
    let session = service.create(CreateRequest {
        app_name: "test_app".to_string(),
        user_id: "user1".to_string(),
        session_id: Some("session1".to_string()),
        state: HashMap::new(),
    }).await?;
    
    println!("✅ Created session: {}", session.id());
    println!("   App: {}", session.app_name());
    println!("   User: {}", session.user_id());
    
    // Retrieve the session
    let retrieved = service.get(GetRequest {
        app_name: "test_app".to_string(),
        user_id: "user1".to_string(),
        session_id: "session1".to_string(),
        num_recent_events: None,
        after: None,
    }).await?;
    
    println!("✅ Retrieved session: {}", retrieved.id());
    
    // List sessions
    let sessions = service.list(ListRequest {
        app_name: "test_app".to_string(),
        user_id: "user1".to_string(),
    }).await?;
    
    println!("✅ Found {} session(s)", sessions.len());
    println!("\n📁 Database file created at: {}", db_path);
    println!("   You can inspect it with: sqlite3 {}", db_path);
    
    Ok(())
}
