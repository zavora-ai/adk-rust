use std::{fs, path::Path};

use adk_deploy::{
    BundleBuilder, DeployClient, DeployClientConfig, DeploymentManifest, PushDeploymentRequest,
    SecretSetRequest,
};
use anyhow::{Context, Result, anyhow};
use keyring::Entry;

use crate::cli::{DeployCommands, DeploySecretCommands};

pub async fn run(command: DeployCommands) -> Result<()> {
    match command {
        DeployCommands::Login { endpoint, token } => login(&endpoint, &token).await,
        DeployCommands::Logout => logout(),
        DeployCommands::Init { path, agent_name, binary } => {
            init_manifest(&path, agent_name.as_deref(), binary.as_deref())
        }
        DeployCommands::Validate { path } => validate_manifest(&path),
        DeployCommands::Build { path } => build_bundle(&path),
        DeployCommands::Push { path, env, workspace } => {
            push_bundle(&path, &env, workspace.as_deref()).await
        }
        DeployCommands::Status { env, agent } => status(&env, agent.as_deref()).await,
        DeployCommands::History { env, agent } => history(&env, agent.as_deref()).await,
        DeployCommands::Metrics { env, agent } => metrics(&env, agent.as_deref()).await,
        DeployCommands::Rollback { deployment_id } => rollback(&deployment_id).await,
        DeployCommands::Promote { deployment_id } => promote(&deployment_id).await,
        DeployCommands::Secret { command } => secret(command).await,
    }
}

const DEPLOY_KEYRING_SERVICE: &str = "adk-rust-deploy";

async fn login(endpoint: &str, token: &str) -> Result<()> {
    let config = DeployClientConfig {
        endpoint: endpoint.to_string(),
        token: Some(token.to_string()),
        workspace_id: None,
    };
    let client = DeployClient::new(config);
    let session = client.auth_session().await?;
    save_deploy_token(endpoint, token)?;
    let mut persisted = DeployClientConfig::load()?;
    persisted.endpoint = endpoint.to_string();
    persisted.workspace_id = Some(session.workspace_id.clone());
    persisted.token = None;
    persisted.save()?;
    println!(
        "Stored deploy token. User: {} Workspace: {} ({})",
        session.user_id, session.workspace_name, session.workspace_id
    );
    Ok(())
}

fn logout() -> Result<()> {
    let mut config = DeployClientConfig::load()?;
    delete_deploy_token(&config.endpoint)?;
    config.workspace_id = None;
    config.token = None;
    config.save()?;
    println!("Removed deploy credentials for {}", config.endpoint);
    Ok(())
}

fn init_manifest(path: &str, agent_name: Option<&str>, binary: Option<&str>) -> Result<()> {
    if Path::new(path).exists() {
        return Err(anyhow!("manifest already exists at {path}"));
    }
    let mut manifest = DeploymentManifest::default();
    if let Some(agent_name) = agent_name {
        manifest.agent.name = agent_name.to_string();
    }
    if let Some(binary) = binary {
        manifest.agent.binary = binary.to_string();
    }
    manifest.agent.description = Some("ADK deployment manifest".to_string());
    let toml = manifest.to_toml_string()?;
    fs::write(path, toml)?;
    println!("Wrote starter manifest to {path}");
    Ok(())
}

fn validate_manifest(path: &str) -> Result<()> {
    let manifest = DeploymentManifest::from_path(Path::new(path))?;
    manifest.validate()?;
    println!("Manifest valid: agent={} strategy={:?}", manifest.agent.name, manifest.strategy.kind);
    Ok(())
}

fn build_bundle(path: &str) -> Result<()> {
    let manifest_path = Path::new(path);
    let manifest = DeploymentManifest::from_path(manifest_path)?;
    let artifact = BundleBuilder::new(manifest_path, manifest).build()?;
    println!("Bundle: {}", artifact.bundle_path.display());
    println!("Checksum: {}", artifact.checksum_sha256);
    println!("Binary: {}", artifact.binary_path.display());
    Ok(())
}

async fn push_bundle(path: &str, env: &str, workspace: Option<&str>) -> Result<()> {
    let manifest_path = Path::new(path);
    let mut manifest = DeploymentManifest::from_path(manifest_path)?;
    if manifest.source.is_none() {
        manifest.source = Some(adk_deploy::SourceInfo {
            kind: "cli".to_string(),
            project_id: None,
            project_name: None,
        });
    }
    let artifact = BundleBuilder::new(manifest_path, manifest.clone()).build()?;
    let client = load_client()?;
    let response = client
        .push_deployment(&PushDeploymentRequest {
            workspace_id: workspace
                .map(str::to_string)
                .or_else(|| client.config().workspace_id.clone()),
            environment: env.to_string(),
            manifest,
            bundle_path: artifact.bundle_path.display().to_string(),
            checksum_sha256: artifact.checksum_sha256.clone(),
            binary_path: Some(artifact.binary_path.display().to_string()),
        })
        .await?;
    println!(
        "Deployment created: {} {} {}",
        response.deployment.id, response.deployment.agent_name, response.deployment.version
    );
    println!("Endpoint: {}", response.deployment.endpoint_url);
    Ok(())
}

async fn status(env: &str, agent: Option<&str>) -> Result<()> {
    let client = load_client()?;
    let response = client.status(env, agent).await?;
    println!(
        "{} {} {} {}",
        response.deployment.agent_name,
        response.deployment.version,
        response.deployment.environment,
        response.deployment.rollout_phase
    );
    println!(
        "latency: p95={} error_rate={} active_connections={}",
        response.metrics.latency_p95,
        response.metrics.error_rate,
        response.metrics.active_connections
    );
    Ok(())
}

async fn history(env: &str, agent: Option<&str>) -> Result<()> {
    let client = load_client()?;
    let response = client.history(env, agent).await?;
    for item in response.items {
        println!(
            "{} {} {} {:?} {}",
            item.id, item.agent_name, item.version, item.strategy, item.created_at
        );
    }
    Ok(())
}

async fn metrics(env: &str, agent: Option<&str>) -> Result<()> {
    let client = load_client()?;
    let response = client.status(env, agent).await?;
    println!("request_rate={}", response.metrics.request_rate);
    println!("latency_p50={}", response.metrics.latency_p50);
    println!("latency_p95={}", response.metrics.latency_p95);
    println!("latency_p99={}", response.metrics.latency_p99);
    println!("error_rate={}", response.metrics.error_rate);
    Ok(())
}

async fn rollback(deployment_id: &str) -> Result<()> {
    let client = load_client()?;
    let response = client.rollback(deployment_id).await?;
    println!(
        "Rolled back {} to {} ({})",
        response.deployment.agent_name,
        response.deployment.version,
        response.deployment.rollout_phase
    );
    Ok(())
}

async fn promote(deployment_id: &str) -> Result<()> {
    let client = load_client()?;
    let response = client.promote(deployment_id).await?;
    println!(
        "Promoted {} {} ({})",
        response.deployment.agent_name,
        response.deployment.version,
        response.deployment.rollout_phase
    );
    Ok(())
}

async fn secret(command: DeploySecretCommands) -> Result<()> {
    let client = load_client()?;
    match command {
        DeploySecretCommands::Set { env, key, value } => {
            client.set_secret(&SecretSetRequest { environment: env, key, value }).await?;
            println!("Secret stored");
        }
        DeploySecretCommands::List { env } => {
            let response = client.list_secrets(&env).await?;
            for key in response.keys {
                println!("{key}");
            }
        }
        DeploySecretCommands::Delete { env, key } => {
            client.delete_secret(&env, &key).await?;
            println!("Secret deleted");
        }
    }
    Ok(())
}

fn load_client() -> Result<DeployClient> {
    let mut config = DeployClientConfig::load()?;
    if let Some(stored) = load_deploy_token(&config.endpoint)? {
        config.token = Some(stored);
    } else if let Some(legacy_token) = config.token.clone() {
        save_deploy_token(&config.endpoint, &legacy_token)?;
        config.token = Some(legacy_token);
        let mut persisted = config.clone();
        persisted.token = None;
        persisted.save()?;
    }
    if config.token.is_none() {
        return Err(anyhow!(
            "no deploy token configured for {}. Run `adk deploy login --endpoint ... --token ...`",
            config.endpoint
        ));
    }
    Ok(DeployClient::new(config))
}

fn keyring_entry(endpoint: &str) -> Result<Entry> {
    Entry::new(DEPLOY_KEYRING_SERVICE, endpoint)
        .with_context(|| format!("failed to initialize deploy credential storage for {endpoint}"))
}

fn load_deploy_token(endpoint: &str) -> Result<Option<String>> {
    match keyring_entry(endpoint)?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(anyhow!("failed to load deploy token from keyring: {error}")),
    }
}

fn save_deploy_token(endpoint: &str, token: &str) -> Result<()> {
    keyring_entry(endpoint)?
        .set_password(token)
        .with_context(|| format!("failed to save deploy token for {endpoint}"))
}

fn delete_deploy_token(endpoint: &str) -> Result<()> {
    match keyring_entry(endpoint)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(anyhow!("failed to delete deploy token from keyring: {error}")),
    }
}
