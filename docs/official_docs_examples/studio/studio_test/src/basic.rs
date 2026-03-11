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

    let _app: Router<()> = Router::new().nest("/api", api_routes()).with_state(state);
    println!("✓ api_routes() works");

    // From docs: Repo-local project storage convention
    // Validates that FileStorage works with a relative repo-local path
    let repo_local_dir = temp_dir.join(".adk-studio/projects");
    std::fs::create_dir_all(&repo_local_dir)?;
    let repo_local_storage = FileStorage::new(repo_local_dir).await?;
    println!("✓ FileStorage::new works with repo-local path (.adk-studio/projects)");

    let _repo_local_state = AppState::new(repo_local_storage);
    println!("✓ AppState::new works with repo-local storage");

    // Cleanup
    std::fs::remove_dir_all(&temp_dir).ok();

    println!("\n=== All studio tests passed! ===");
    Ok(())
}
