//! The website API's generated OpenAPI document and TypeScript types match the Rust source
//! (spec 3.5 §8.2). `scripts/check-surface-api.sh --write` regenerates them.

#![cfg(feature = "website")]

use std::path::PathBuf;

#[test]
fn the_generated_website_api_matches_the_rust_types() {
    let openapi = format!(
        "{}\n",
        serde_json::to_string_pretty(&adk_surface::web::website::openapi()).unwrap()
    );
    let typescript = adk_surface::web::website::typescript();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/generated");
    if std::env::var_os("ADK_SURFACE_WRITE_API").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("website-openapi.json"), &openapi).unwrap();
        std::fs::write(dir.join("website.ts"), &typescript).unwrap();
        return;
    }
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    assert!(
        read("website-openapi.json") == openapi && read("website.ts") == typescript,
        "adk-surface/web/src/generated/website* is stale: run `bash scripts/check-surface-api.sh \
         --write` and commit the result"
    );
}

#[test]
fn every_route_the_router_serves_is_documented() {
    let document = adk_surface::web::website::openapi();
    let paths = document["paths"].as_object().unwrap();
    use adk_surface::web::website::paths;
    for path in [paths::SETUP, paths::CLAIM] {
        assert!(paths.contains_key(path), "{path}");
    }
    for path in [
        paths::ROOT,
        paths::ONBOARDING,
        paths::CHARTER,
        paths::SITE,
        paths::REVIEW,
        paths::EDITOR,
        paths::DRAFT,
        paths::MEDIA,
        paths::OPENAPI,
    ] {
        assert!(paths.contains_key(&format!("/api{path}")), "{path}");
    }
    let schemas = document["components"]["schemas"].as_object().unwrap();
    for schema in [
        "OnboardingView",
        "CharterReview",
        "SiteViewModel",
        "ReviewPage",
        "EditorHome",
        "DraftView",
    ] {
        assert!(schemas.contains_key(schema), "{schema}");
    }
}
