//! Studio doc-test - validates studio.md documentation

use adk_studio::{AppState, FileStorage, api_routes};
use axum::Router;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Studio Doc-Test ===\n");

    // From docs: Programmatic usage
    let temp_dir = std::env::temp_dir().join("studio_test_projects");
    std::fs::create_dir_all(&temp_dir)?;

    let storage = FileStorage::new(temp_dir.clone()).await?;
    println!("✓ FileStorage::new works");

    let state = AppState::new(storage);
    println!("✓ AppState::new works");

    let _app: Router<()> = Router::new()
        .nest("/api", api_routes())
        .with_state(state);
    println!("✓ api_routes() works");

    // Cleanup
    std::fs::remove_dir_all(&temp_dir).ok();

    println!("\n=== All studio tests passed! ===");
    Ok(())
}
