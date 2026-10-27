//! The screen actions' generated OpenAPI document and TypeScript types match the Rust source
//! (spec 3.3 §2 item 7, §6). `scripts/check-surface-api.sh --write` regenerates them.

#![cfg(all(feature = "verb-ui", feature = "web"))]

use std::path::PathBuf;

#[test]
fn the_generated_verbs_api_matches_the_rust_types() {
    let openapi =
        format!("{}\n", serde_json::to_string_pretty(&adk_surface::web::verbs::openapi()).unwrap());
    let typescript = adk_surface::web::verbs::typescript();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/generated");
    if std::env::var_os("ADK_SURFACE_WRITE_API").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("verbs-openapi.json"), &openapi).unwrap();
        std::fs::write(dir.join("verbs.ts"), &typescript).unwrap();
        return;
    }
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    assert!(
        read("verbs-openapi.json") == openapi && read("verbs.ts") == typescript,
        "adk-surface/web/src/generated/verbs* is stale: run `bash scripts/check-surface-api.sh \
         --write` and commit the result"
    );
}

#[test]
fn every_route_the_router_serves_is_documented() {
    use adk_surface::web::verbs::paths;
    let document = adk_surface::web::verbs::openapi();
    let routes = document["paths"].as_object().unwrap();
    for path in [paths::VERBS, paths::CALL, paths::OPENAPI] {
        assert!(routes.contains_key(&format!("/api{path}")), "{path}");
    }
    let schemas = document["components"]["schemas"].as_object().unwrap();
    for schema in [
        "VerbsResponse",
        "VerbCallRequest",
        "VerbIntent",
        "VerbDone",
        "ComingUpItem",
        "SnoozeChoice",
    ] {
        assert!(schemas.contains_key(schema), "{schema}");
    }
}
