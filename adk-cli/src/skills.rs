use crate::cli::SkillsCommands;
use adk_skill::{SelectionPolicy, load_skill_index, select_skills};
use anyhow::{Result, anyhow};
use serde_json::json;
use std::path::PathBuf;

pub async fn run(command: SkillsCommands) -> Result<()> {
    match command {
        SkillsCommands::List { path, json: as_json } => list(&path, as_json),
        SkillsCommands::Validate { path, json: as_json } => validate(&path, as_json),
        SkillsCommands::Match {
            query,
            path,
            top_k,
            min_score,
            include_tags,
            exclude_tags,
            json: as_json,
        } => match_skills(&query, &path, top_k, min_score, include_tags, exclude_tags, as_json),
        #[cfg(feature = "vertex-skill-registry")]
        SkillsCommands::Search { query, top_k, project, location, endpoint, json: as_json } => {
            search(&query, top_k, project, location, endpoint, as_json).await
        }
        #[cfg(feature = "vertex-skill-registry")]
        SkillsCommands::Pull { name, revision, dir, project, location, endpoint } => {
            pull(&name, revision.as_deref(), &dir, project, location, endpoint).await
        }
    }
}

fn list(path: &str, as_json: bool) -> Result<()> {
    let root = PathBuf::from(path);
    let index = load_skill_index(&root).map_err(|e| anyhow!(e.to_string()))?;

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "count": index.len(),
                "skills": index.summaries(),
            }))?
        );
    } else {
        println!("Found {} skill(s)", index.len());
        for skill in index.summaries() {
            println!("- {}: {} ({})", skill.name, skill.description, skill.path.display());
        }
    }

    Ok(())
}

fn validate(path: &str, as_json: bool) -> Result<()> {
    let root = PathBuf::from(path);
    match load_skill_index(&root) {
        Ok(index) => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "valid": true,
                        "count": index.len(),
                        "skills": index.summaries(),
                    }))?
                );
            } else {
                println!("Skills validation succeeded ({} skill(s))", index.len());
            }
            Ok(())
        }
        Err(err) => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "valid": false,
                        "error": err.to_string(),
                    }))?
                );
            } else {
                eprintln!("Skills validation failed: {}", err);
            }
            Err(anyhow!(err.to_string()))
        }
    }
}

fn match_skills(
    query: &str,
    path: &str,
    top_k: usize,
    min_score: f32,
    include_tags: Vec<String>,
    exclude_tags: Vec<String>,
    as_json: bool,
) -> Result<()> {
    let root = PathBuf::from(path);
    let index = load_skill_index(&root).map_err(|e| anyhow!(e.to_string()))?;
    let policy = SelectionPolicy { top_k, min_score, include_tags, exclude_tags };
    let matches = select_skills(&index, query, &policy);

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "query": query,
                "count": matches.len(),
                "matches": matches,
            }))?
        );
    } else {
        println!("Matched {} skill(s) for query: {}", matches.len(), query);
        for item in matches {
            println!("- {} (score {:.2})", item.skill.name, item.score);
        }
    }

    Ok(())
}

// ===== Vertex AI Skill Registry (read-only) =====

/// `skills search`: semantic search against the Skill Registry.
#[cfg(feature = "vertex-skill-registry")]
async fn search(
    query: &str,
    top_k: Option<u32>,
    project: Option<String>,
    location: Option<String>,
    endpoint: Option<String>,
    as_json: bool,
) -> Result<()> {
    use adk_skill::registry::SkillRegistryClient;

    let config = resolve_registry_config(project, location, endpoint)?;
    let client = SkillRegistryClient::new_with_adc(config)?;
    let results = run_search(&client, query, top_k).await?;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        println!("Found {} skill(s) for query: {query}", results.len());
        for (rank, hit) in results.iter().enumerate() {
            let short = hit.skill_name.rsplit('/').next().unwrap_or(&hit.skill_name);
            println!("{}. {short}: {} ({})", rank + 1, hit.description, hit.skill_name);
        }
    }
    Ok(())
}

/// The search pipeline behind `skills search`, separated from the printing
/// shell so the contract test can drive the real path against a mock server.
#[cfg(feature = "vertex-skill-registry")]
async fn run_search(
    client: &adk_skill::registry::SkillRegistryClient,
    query: &str,
    top_k: Option<u32>,
) -> Result<Vec<adk_skill::registry::RetrievedSkill>> {
    Ok(client.search_skills(query, top_k).await?)
}

/// `skills pull`: materialize a skill package into a local directory.
#[cfg(feature = "vertex-skill-registry")]
async fn pull(
    name: &str,
    revision: Option<&str>,
    dir: &str,
    project: Option<String>,
    location: Option<String>,
    endpoint: Option<String>,
) -> Result<()> {
    use adk_skill::registry::SkillRegistryClient;

    let config = resolve_registry_config(project, location, endpoint)?;
    let client = SkillRegistryClient::new_with_adc(config)?;
    let (skill_id, written) = run_pull(&client, name, revision, &PathBuf::from(dir)).await?;

    let pin = revision.map(|rev| format!("revision {rev}")).unwrap_or_else(|| "latest".to_string());
    println!("Pulled skill `{skill_id}` ({pin}) — {} file(s):", written.len());
    for path in written {
        println!("- {}", path.display());
    }
    Ok(())
}

/// The pull pipeline behind `skills pull`, separated from the printing shell
/// so the contract test can drive the real path against a mock server.
///
/// Materializes the verified package under `{dir}/{skill-id}/` via the safe
/// extract-to-dir path and returns the skill ID and written file paths.
#[cfg(feature = "vertex-skill-registry")]
async fn run_pull(
    client: &adk_skill::registry::SkillRegistryClient,
    name: &str,
    revision: Option<&str>,
    dir: &std::path::Path,
) -> Result<(String, Vec<PathBuf>)> {
    let content = match revision {
        Some(revision) => client.fetch_skill_revision_content(name, revision).await?,
        None => client.fetch_skill_content(name).await?,
    };
    let skill_id = content
        .skill
        .name
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or_else(|| name.rsplit('/').next().unwrap_or(name))
        .to_string();
    let target = dir.join(&skill_id);
    let written = content.write_to_dir(&target).map_err(|e| anyhow!(e.to_string()))?;
    Ok((skill_id, written))
}

/// Resolves project/location from flags with environment fallback.
#[cfg(feature = "vertex-skill-registry")]
fn resolve_registry_config(
    project: Option<String>,
    location: Option<String>,
    endpoint: Option<String>,
) -> Result<adk_skill::registry::SkillRegistryConfig> {
    resolve_registry_config_with(project, location, endpoint, |key| std::env::var(key).ok())
}

#[cfg(feature = "vertex-skill-registry")]
fn resolve_registry_config_with(
    project: Option<String>,
    location: Option<String>,
    endpoint: Option<String>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<adk_skill::registry::SkillRegistryConfig> {
    let resolve = |flag: Option<String>, key: &str| {
        flag.or_else(|| env(key)).map(|value| value.trim().to_string()).filter(|v| !v.is_empty())
    };
    let project = resolve(project, "GOOGLE_CLOUD_PROJECT");
    let location = resolve(location, "GOOGLE_CLOUD_LOCATION");
    match (project, location) {
        (Some(project), Some(location)) => {
            let mut config = adk_skill::registry::SkillRegistryConfig::new(project, location);
            if let Some(endpoint) = endpoint {
                config = config.with_endpoint(endpoint);
            }
            Ok(config)
        }
        (project, location) => {
            let mut missing = Vec::new();
            if project.is_none() {
                missing.push("--project (or $GOOGLE_CLOUD_PROJECT)");
            }
            if location.is_none() {
                missing.push("--location (or $GOOGLE_CLOUD_LOCATION)");
            }
            Err(anyhow!("missing {}", missing.join(" and ")))
        }
    }
}

#[cfg(all(test, feature = "vertex-skill-registry"))]
mod skill_registry_tests {
    use super::*;
    use crate::cli::{Cli, Commands};
    use adk_skill::registry::SkillRegistryClient;
    use axum::extract::Path as AxumPath;
    use axum::routing::get;
    use axum::{Json, Router};
    use base64::Engine as _;
    use clap::Parser;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::io::Write;

    const PARENT: &str = "projects/test-project/locations/us-central1";
    const SKILL_MD: &str =
        "---\nname: report-writer\ndescription: Writes reports.\n---\nDraft the report.\n";

    /// Parses a full command line into the skills subcommand.
    fn parse(argv: &[&str]) -> SkillsCommands {
        let cli = Cli::try_parse_from(argv).expect("argv parses");
        match cli.command {
            Some(Commands::Skills { command }) => command,
            _ => panic!("argv did not parse to a skills command"),
        }
    }

    #[test]
    fn arg_parse_covers_the_search_surface() {
        let command = parse(&[
            "adk-rust",
            "skills",
            "search",
            "quarterly reporting",
            "--top-k",
            "5",
            "--project",
            "p",
            "--location",
            "us-central1",
            "--endpoint",
            "https://example.com",
            "--json",
        ]);
        match command {
            SkillsCommands::Search { query, top_k, project, location, endpoint, json } => {
                assert_eq!(query, "quarterly reporting");
                assert_eq!(top_k, Some(5));
                assert_eq!(project.as_deref(), Some("p"));
                assert_eq!(location.as_deref(), Some("us-central1"));
                assert_eq!(endpoint.as_deref(), Some("https://example.com"));
                assert!(json);
            }
            _ => panic!("expected skills search"),
        }
    }

    #[test]
    fn arg_parse_covers_the_pull_surface_and_defaults() {
        let command = parse(&["adk-rust", "skills", "pull", "report-writer"]);
        match command {
            SkillsCommands::Pull { name, revision, dir, project, location, endpoint } => {
                assert_eq!(name, "report-writer");
                assert_eq!(revision, None);
                assert_eq!(dir, ".skills");
                assert_eq!(project, None);
                assert_eq!(location, None);
                assert_eq!(endpoint, None);
            }
            _ => panic!("expected skills pull"),
        }

        let command = parse(&[
            "adk-rust",
            "skills",
            "pull",
            "report-writer",
            "--revision",
            "3",
            "--dir",
            "vendored-skills",
        ]);
        match command {
            SkillsCommands::Pull { revision, dir, .. } => {
                assert_eq!(revision.as_deref(), Some("3"));
                assert_eq!(dir, "vendored-skills");
            }
            _ => panic!("expected skills pull"),
        }
    }

    #[test]
    fn config_resolution_prefers_flags_and_falls_back_to_env() {
        let env = |key: &str| match key {
            "GOOGLE_CLOUD_PROJECT" => Some("env-project".to_string()),
            "GOOGLE_CLOUD_LOCATION" => Some("europe-west4".to_string()),
            _ => None,
        };

        let config =
            resolve_registry_config_with(Some("flag-project".to_string()), None, None, env)
                .expect("flags + env resolve");
        assert_eq!(config.project_id, "flag-project");
        assert_eq!(config.location, "europe-west4");

        let error = resolve_registry_config_with(None, None, None, |_| None)
            .expect_err("missing config must fail");
        let message = error.to_string();
        assert!(message.contains("--project"), "unexpected error: {message}");
        assert!(message.contains("--location"), "unexpected error: {message}");
    }

    fn skill_fixture() -> Value {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data) in
            [("SKILL.md", SKILL_MD.as_bytes()), ("references/data.json", br#"{"n": 1}"#.as_slice())]
        {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .expect("start zip entry");
            writer.write_all(data).expect("write zip entry");
        }
        let zip_bytes = writer.finish().expect("finish zip").into_inner();
        json!({
            "name": format!("{PARENT}/skills/report-writer"),
            "displayName": "report-writer",
            "description": "Writes reports.",
            "zippedFilesystem": base64::engine::general_purpose::STANDARD.encode(&zip_bytes),
            "sha256": format!("{:x}", Sha256::digest(&zip_bytes)),
            "state": "ACTIVE",
        })
    }

    async fn serve_mock() -> (String, tokio::task::JoinHandle<()>) {
        let prefix = format!("/v1beta1/{PARENT}");
        let app = Router::new()
            .route(
                &format!("{prefix}/{{skills_verb}}"),
                get(|| async {
                    Json(json!({
                        "retrievedSkills": [
                            {
                                "skillName": format!("{PARENT}/skills/report-writer"),
                                "description": "Writes reports.",
                            },
                        ],
                    }))
                }),
            )
            .route(
                &format!("{prefix}/skills/{{skill}}"),
                get(|AxumPath(skill): AxumPath<String>| async move {
                    assert_eq!(skill, "report-writer");
                    Json(skill_fixture())
                }),
            )
            .route(
                &format!("{prefix}/skills/{{skill}}/revisions/{{revision}}"),
                get(|AxumPath((skill, revision)): AxumPath<(String, String)>| async move {
                    Json(json!({
                        "name": format!("{PARENT}/skills/{skill}/revisions/{revision}"),
                        "state": "ACTIVE",
                        "skill": skill_fixture(),
                    }))
                }),
            );
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock skill registry server should run");
        });
        (format!("http://{addr}"), server)
    }

    fn mock_client(endpoint: &str) -> SkillRegistryClient {
        // run_search/run_pull build their client with ADC, which is
        // unavailable in CI — so the tests drive the same pipeline through an
        // explicit-credential client against the mock, exactly like the
        // deploy agent-engine contract test.
        let config = resolve_registry_config_with(
            Some("test-project".to_string()),
            Some("us-central1".to_string()),
            Some(endpoint.to_string()),
            |_| None,
        )
        .expect("config resolves");
        let credentials =
            google_cloud_auth::credentials::api_key_credentials::Builder::new("test-key").build();
        SkillRegistryClient::with_credentials(config, credentials).expect("build mock client")
    }

    #[tokio::test]
    async fn search_returns_ranked_hits_from_the_mock_registry() {
        let (endpoint, server) = serve_mock().await;
        let client = mock_client(&endpoint);

        let results = run_search(&client, "reporting", Some(3)).await.expect("search succeeds");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].skill_name, format!("{PARENT}/skills/report-writer"));
        assert_eq!(results[0].description, "Writes reports.");

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn pull_materializes_the_package_under_the_skill_directory() {
        let (endpoint, server) = serve_mock().await;
        let client = mock_client(&endpoint);
        let dir = tempfile::tempdir().expect("create temp dir");

        let (skill_id, written) =
            run_pull(&client, "report-writer", None, dir.path()).await.expect("pull succeeds");

        assert_eq!(skill_id, "report-writer");
        let root = dir.path().join("report-writer");
        assert_eq!(written, vec![root.join("SKILL.md"), root.join("references/data.json")]);
        assert_eq!(
            std::fs::read_to_string(root.join("SKILL.md")).expect("read SKILL.md"),
            SKILL_MD,
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn pull_with_a_revision_pin_uses_the_revision_snapshot() {
        let (endpoint, server) = serve_mock().await;
        let client = mock_client(&endpoint);
        let dir = tempfile::tempdir().expect("create temp dir");

        let (skill_id, written) = run_pull(&client, "report-writer", Some("3"), dir.path())
            .await
            .expect("pinned pull succeeds");

        assert_eq!(skill_id, "report-writer");
        assert_eq!(written.len(), 2);

        server.abort();
        let _ = server.await;
    }
}
