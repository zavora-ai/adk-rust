//! The Runs tab's generated OpenAPI document and TypeScript types match the Rust source
//! (spec 3.3 §12). `scripts/check-surface-api.sh --write` regenerates them.

#![cfg(all(feature = "web", feature = "workflow"))]

use std::path::PathBuf;

#[test]
fn the_generated_workflow_api_matches_the_rust_types() {
    let openapi = format!(
        "{}\n",
        serde_json::to_string_pretty(&adk_surface::web::workflow::openapi()).unwrap()
    );
    let typescript = adk_surface::web::workflow::typescript();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/generated");
    if std::env::var_os("ADK_SURFACE_WRITE_API").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("workflow-openapi.json"), &openapi).unwrap();
        std::fs::write(dir.join("workflow.ts"), &typescript).unwrap();
        return;
    }
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    assert!(
        read("workflow-openapi.json") == openapi && read("workflow.ts") == typescript,
        "adk-surface/web/src/generated/workflow* is stale: run `bash scripts/check-surface-api.sh \
         --write` and commit the result"
    );
}

#[test]
fn every_route_the_router_serves_is_documented() {
    use adk_surface::web::workflow::paths;
    let document = adk_surface::web::workflow::openapi();
    let documented = document["paths"].as_object().unwrap();
    let mut missing = Vec::new();
    for path in [
        paths::GRAPHS,
        paths::RUNS,
        paths::RUN,
        paths::CHECKPOINT,
        paths::FEED,
        paths::FORK,
        paths::START,
        paths::OPENAPI,
    ] {
        if !documented.contains_key(&format!("/api{path}")) {
            missing.push(path);
        }
    }
    assert_eq!(missing, Vec::<&str>::new());
    let schemas = document["components"]["schemas"].as_object().unwrap();
    let absent: Vec<&str> = [
        "GraphsResponse",
        "WorkflowRun",
        "RunsResponse",
        "StepDetail",
        "ForkRequest",
        "ForkOutcome",
    ]
    .into_iter()
    .filter(|schema| !schemas.contains_key(*schema))
    .collect();
    assert_eq!(absent, Vec::<&str>::new());
}
