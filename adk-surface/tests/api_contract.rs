//! The web API's generated OpenAPI document and TypeScript types match the Rust source
//! (spec 3.3 §6). `scripts/check-surface-api.sh --write` regenerates them.

#![cfg(feature = "web")]

use std::path::PathBuf;

fn generated() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/generated")
}

#[test]
fn the_generated_api_matches_the_rust_types() {
    let openapi =
        format!("{}\n", serde_json::to_string_pretty(&adk_surface::web::openapi()).unwrap());
    let typescript = adk_surface::web::typescript();
    let dir = generated();
    if std::env::var_os("ADK_SURFACE_WRITE_API").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("openapi.json"), &openapi).unwrap();
        std::fs::write(dir.join("api.ts"), &typescript).unwrap();
        return;
    }
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    assert!(
        read("openapi.json") == openapi && read("api.ts") == typescript,
        "adk-surface/web/src/generated is stale: run `bash scripts/check-surface-api.sh --write` \
         and commit the result"
    );
}
