//! Request shapes the Managed Agents client sends, captured from a local server.
//!
//! Run with:
//! ```bash
//! cargo test -p adk-anthropic --features managed-agents --test managed_agents_requests
//! ```

#![cfg(feature = "managed-agents")]

use adk_anthropic::managed_agents::{
    CreateEnvironmentParams, ManagedAgentsClient, ToolConfig, UpdateCredentialParams,
    UpdateMemoryParams,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Serves one request with `response` and reports its request line and body.
async fn capture_one(response: Value) -> (String, oneshot::Receiver<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let (head, body) = loop {
            let read = socket.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..read]);
            let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&request[..end]).to_string();
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or_default();
            if read == 0 || request.len() >= end + 4 + length {
                break (head, request[end + 4..end + 4 + length].to_vec());
            }
        };
        let reply_body = response.to_string();
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply_body}",
            reply_body.len()
        );
        socket.write_all(reply.as_bytes()).await.unwrap();
        let request_line = head.lines().next().unwrap_or_default().to_string();
        let sent =
            if body.is_empty() { Value::Null } else { serde_json::from_slice(&body).unwrap() };
        let _ = sender.send((request_line, sent));
    });
    (format!("http://{address}"), receiver)
}

fn client(base_url: &str) -> ManagedAgentsClient {
    ManagedAgentsClient::new("test-key").unwrap().with_base_url(base_url).unwrap()
}

#[tokio::test]
async fn update_credential_posts_to_the_credential() {
    let (base_url, sent) =
        capture_one(json!({"id": "vcrd_1", "type": "vault_credential", "vault_id": "vlt_1"})).await;

    let credential = client(&base_url)
        .update_credential(
            "vlt_1",
            "vcrd_1",
            UpdateCredentialParams { auth: json!({"type": "static_bearer", "token": "new"}) },
        )
        .await
        .unwrap();

    let (request_line, body) = sent.await.unwrap();
    assert_eq!(request_line, "POST /v1/vaults/vlt_1/credentials/vcrd_1 HTTP/1.1");
    assert_eq!(body, json!({"auth": {"type": "static_bearer", "token": "new"}}));
    assert_eq!(credential.id, "vcrd_1");
}

#[tokio::test]
async fn update_memory_posts_to_the_memory() {
    let (base_url, sent) = capture_one(json!({"id": "mem_1", "type": "memory"})).await;

    client(&base_url)
        .update_memory(
            "memstore_1",
            "mem_1",
            UpdateMemoryParams {
                content: None,
                path: Some("/archive/notes.md".into()),
                precondition: None,
            },
        )
        .await
        .unwrap();

    let (request_line, body) = sent.await.unwrap();
    assert_eq!(request_line, "POST /v1/memory_stores/memstore_1/memories/mem_1 HTTP/1.1");
    assert_eq!(body, json!({"path": "/archive/notes.md"}));
}

#[tokio::test]
async fn default_cloud_environment_denies_network_egress() {
    let (base_url, sent) = capture_one(json!({"id": "env_1", "name": "sandbox"})).await;

    client(&base_url).create_environment(CreateEnvironmentParams::cloud("sandbox")).await.unwrap();

    let (request_line, body) = sent.await.unwrap();
    assert_eq!(request_line, "POST /v1/environments HTTP/1.1");
    assert_eq!(
        body,
        json!({
            "name": "sandbox",
            "config": {
                "type": "cloud",
                "networking": {
                    "type": "limited",
                    "allowed_hosts": [],
                    "allow_package_managers": false,
                    "allow_mcp_servers": false
                }
            }
        })
    );
}

#[test]
fn environment_networking_is_widened_only_on_request() {
    assert_eq!(
        CreateEnvironmentParams::cloud_limited("api", ["api.example.com"]).config["networking"],
        json!({
            "type": "limited",
            "allowed_hosts": ["api.example.com"],
            "allow_package_managers": false,
            "allow_mcp_servers": false
        })
    );
    assert_eq!(
        CreateEnvironmentParams::cloud_unrestricted("open").config["networking"],
        json!({"type": "unrestricted"})
    );
}

#[test]
fn agent_toolset_enables_web_tools_only_on_request() {
    let web_off = json!([
        {"name": "web_fetch", "enabled": false},
        {"name": "web_search", "enabled": false}
    ]);
    assert_eq!(
        ToolConfig::agent_toolset(),
        json!({"type": "agent_toolset_20260401", "configs": web_off})
    );
    assert_eq!(
        ToolConfig::agent_toolset_with_policy("always_ask"),
        json!({
            "type": "agent_toolset_20260401",
            "default_config": {"permission_policy": {"type": "always_ask"}},
            "configs": web_off
        })
    );
    assert_eq!(ToolConfig::agent_toolset_with_web(), json!({"type": "agent_toolset_20260401"}));
}
