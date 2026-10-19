//! Serialization conformance tests — validates the service's wire format.
//!
//! This file covers remaining serialization/deserialization gaps not already
//! tested in `user_event_serialization_tests.rs` and
//! `session_event_deserialization_tests.rs`:
//!
//! 1. Agent/CreateAgentParams serialization (snake_case, skip_serializing_if)
//! 2. Environment/CreateEnvironmentParams serialization
//! 3. Session/CreateSessionParams serialization
//! 4. ListResponse deserialization (with/without next_cursor)
//! 5. ToolConfig (builtin vs custom) round-trip
//! 6. PermissionPolicy/PermissionMode serialization
//! 7. Vault/Memory type serialization
//! 8. ModelRef untagged deserialization (shorthand + structured + compatible)
//! 9. SessionEvent unknown type → Unknown variant
//!
//! **Validates: Requirements 13.1, 13.2, 13.3, 13.4, 13.5, 13.6**

use std::collections::HashMap;

use adk_enterprise::{
    Agent, CreateAgentParams, CreateCredentialParams, CreateEnvironmentParams, CreateMemoryParams,
    CreateMemoryStoreParams, CreateSessionParams, CreateVaultParams, Credential, Environment,
    ListResponse, Memory, MemoryStore, MemoryVersion, ModelConfig, ModelRef, PermissionMode,
    PermissionPolicy, Provider, Session, SessionEvent, SessionStatus, ToolConfig,
    UpdateAgentParams, UpdateCredentialParams, UpdateMemoryParams, Usage, Vault,
};
use serde_json::json;

// ═══════════════════════════════════════════════════════════════════════════════
// 1. Agent / CreateAgentParams serialization
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn create_agent_params_minimal_uses_snake_case() {
    let params = CreateAgentParams {
        name: "My Agent".into(),
        model: "gemini-2.5-flash".into(),
        ..Default::default()
    };
    let json = serde_json::to_value(&params).unwrap();

    // Required fields present with snake_case
    assert_eq!(json["name"], "My Agent");
    assert_eq!(json["model"], "gemini-2.5-flash");

    // Optional None fields are skipped
    assert!(json.get("system").is_none());
    assert!(json.get("description").is_none());
    assert!(json.get("permission_policy").is_none());
    assert!(json.get("metadata").is_none());

    // Empty vecs are skipped
    assert!(json.get("tools").is_none());
    assert!(json.get("mcp_servers").is_none());
    assert!(json.get("skills").is_none());
}

#[test]
fn create_agent_params_full_uses_snake_case() {
    let params = CreateAgentParams {
        name: "Full Agent".into(),
        model: ModelRef::structured(Provider::Openai, "gpt-4.1"),
        system: Some("You are helpful.".into()),
        description: Some("A full agent".into()),
        tools: vec![ToolConfig::builtin("bash")],
        mcp_servers: vec![],
        skills: vec![],
        permission_policy: Some(PermissionPolicy { mode: PermissionMode::AutoApprove }),
        metadata: Some(HashMap::from([("env".into(), "prod".into())])),
    };
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["name"], "Full Agent");
    assert_eq!(json["system"], "You are helpful.");
    assert_eq!(json["description"], "A full agent");
    // permission_policy snake_case
    assert_eq!(json["permission_policy"]["mode"], "autoApprove");
    assert_eq!(json["metadata"]["env"], "prod");
    // tools present (non-empty)
    assert_eq!(json["tools"][0]["type"], "builtin");
    assert_eq!(json["tools"][0]["name"], "bash");
    // mcp_servers and skills are empty → skipped
    assert!(json.get("mcp_servers").is_none());
    assert!(json.get("skills").is_none());
}

#[test]
fn update_agent_params_skips_none_fields() {
    let params = UpdateAgentParams { name: Some("New Name".into()), ..Default::default() };
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["name"], "New Name");
    assert!(json.get("model").is_none());
    assert!(json.get("system").is_none());
    assert!(json.get("description").is_none());
    assert!(json.get("tools").is_none());
    assert!(json.get("mcp_servers").is_none());
    assert!(json.get("skills").is_none());
    assert!(json.get("permission_policy").is_none());
    assert!(json.get("metadata").is_none());
}

#[test]
fn agent_response_deserializes_from_api_json() {
    let api_response = json!({
        "id": "agt_abc123",
        "name": "Test Agent",
        "model": "gemini-2.5-flash",
        "system": "You are helpful.",
        "description": null,
        "tools": [
            {"type": "builtin", "name": "bash"},
            {"type": "custom", "name": "calc", "description": "Calculate math", "input_schema": {"type": "object"}}
        ],
        "mcp_servers": [],
        "skills": [{"skill_id": "sk_001"}],
        "permission_policy": {"mode": "autoApprove"},
        "metadata": {"team": "platform"},
        "version": 3,
        "created_at": "2026-01-15T10:00:00Z",
        "updated_at": "2026-01-15T12:00:00Z",
        "archived_at": null
    });

    let agent: Agent = serde_json::from_value(api_response).unwrap();
    assert_eq!(agent.id, "agt_abc123");
    assert_eq!(agent.name, "Test Agent");
    assert_eq!(agent.version, 3);
    assert_eq!(agent.tools.len(), 2);
    assert_eq!(agent.skills.len(), 1);
    assert_eq!(agent.description, None);
    assert_eq!(agent.archived_at, None);
    assert_eq!(
        agent.permission_policy,
        Some(PermissionPolicy { mode: PermissionMode::AutoApprove })
    );
}

#[test]
fn agent_response_deserializes_with_missing_optional_fields() {
    let minimal_response = json!({
        "id": "agt_min",
        "name": "Minimal",
        "model": "gpt-4.1",
        "version": 1,
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    });

    let agent: Agent = serde_json::from_value(minimal_response).unwrap();
    assert_eq!(agent.id, "agt_min");
    assert_eq!(agent.system, None);
    assert_eq!(agent.description, None);
    assert!(agent.tools.is_empty());
    assert!(agent.mcp_servers.is_empty());
    assert!(agent.skills.is_empty());
    assert_eq!(agent.permission_policy, None);
    assert_eq!(agent.metadata, None);
    assert_eq!(agent.archived_at, None);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 2. Environment / CreateEnvironmentParams serialization
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn create_environment_params_minimal_skips_none() {
    let params = CreateEnvironmentParams {
        name: "dev-env".into(),
        description: None,
        environment_type: None,
        config: None,
    };
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["name"], "dev-env");
    assert!(json.get("description").is_none());
    assert!(json.get("environment_type").is_none());
    assert!(json.get("config").is_none());
}

#[test]
fn create_environment_params_cloud_uses_snake_case() {
    let params = CreateEnvironmentParams::cloud("production");
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["name"], "production");
    assert_eq!(json["environment_type"], "cloud");
    assert!(json["config"].is_object());
}

#[test]
fn create_environment_params_self_hosted() {
    let params = CreateEnvironmentParams::self_hosted("on-prem");
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["name"], "on-prem");
    assert_eq!(json["environment_type"], "self_hosted");
}

#[test]
fn environment_response_deserializes_from_api() {
    let api_response = json!({
        "id": "env_xyz",
        "name": "staging",
        "description": "Staging sandbox",
        "environment_type": "cloud",
        "config": {"type": "cloud", "networking": {"type": "unrestricted"}},
        "created_at": "2026-02-01T00:00:00Z",
        "updated_at": "2026-02-01T00:00:00Z",
        "archived_at": null
    });

    let env: Environment = serde_json::from_value(api_response).unwrap();
    assert_eq!(env.id, "env_xyz");
    assert_eq!(env.name, "staging");
    assert_eq!(env.description, Some("Staging sandbox".to_string()));
    assert_eq!(env.environment_type, Some("cloud".to_string()));
    assert!(env.config.is_some());
    assert_eq!(env.archived_at, None);
}

#[test]
fn environment_response_deserializes_without_optional_fields() {
    let minimal = json!({
        "id": "env_min",
        "name": "basic",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    });

    let env: Environment = serde_json::from_value(minimal).unwrap();
    assert_eq!(env.id, "env_min");
    assert_eq!(env.description, None);
    assert_eq!(env.environment_type, None);
    assert_eq!(env.config, None);
    assert_eq!(env.archived_at, None);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 3. Session / CreateSessionParams serialization
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn create_session_params_minimal_skips_optional() {
    let params = CreateSessionParams { agent_id: "agt_123".into(), ..Default::default() };
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["agent_id"], "agt_123");
    assert!(json.get("environment_id").is_none());
    assert!(json.get("title").is_none());
    assert!(json.get("vault_ids").is_none());
    assert!(json.get("metadata").is_none());
}

#[test]
fn create_session_params_full_uses_snake_case() {
    let params = CreateSessionParams {
        agent_id: "agt_abc".into(),
        environment_id: Some("env_xyz".into()),
        title: Some("My Chat".into()),
        vault_ids: vec!["vault_1".into(), "vault_2".into()],
        metadata: Some(HashMap::from([("source".into(), "api".into())])),
    };
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["agent_id"], "agt_abc");
    assert_eq!(json["environment_id"], "env_xyz");
    assert_eq!(json["title"], "My Chat");
    assert_eq!(json["vault_ids"], json!(["vault_1", "vault_2"]));
    assert_eq!(json["metadata"]["source"], "api");
}

#[test]
fn session_response_deserializes_from_api() {
    let api_response = json!({
        "id": "ses_abc123",
        "agent_id": "agt_001",
        "environment_id": "env_001",
        "status": "running",
        "title": "Debug session",
        "usage": {
            "input_tokens": 1500,
            "output_tokens": 800,
            "total_tokens": 2300,
            "cost_usd": 0.0035
        },
        "created_at": "2026-03-01T10:00:00Z",
        "updated_at": "2026-03-01T10:05:00Z"
    });

    let session: Session = serde_json::from_value(api_response).unwrap();
    assert_eq!(session.id, "ses_abc123");
    assert_eq!(session.agent_id, "agt_001");
    assert_eq!(session.environment_id, Some("env_001".to_string()));
    assert_eq!(session.status, SessionStatus::Running);
    assert_eq!(session.title, Some("Debug session".to_string()));
    let usage = session.usage.unwrap();
    assert_eq!(usage.input_tokens, 1500);
    assert_eq!(usage.output_tokens, 800);
    assert_eq!(usage.total_tokens, 2300);
    assert_eq!(usage.cost_usd, Some(0.0035));
}

#[test]
fn session_response_deserializes_without_optional_fields() {
    let minimal = json!({
        "id": "ses_min",
        "agent_id": "agt_min",
        "status": "idle",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    });

    let session: Session = serde_json::from_value(minimal).unwrap();
    assert_eq!(session.id, "ses_min");
    assert_eq!(session.environment_id, None);
    assert_eq!(session.status, SessionStatus::Idle);
    assert_eq!(session.title, None);
    assert!(session.usage.is_none());
}

#[test]
fn session_status_all_variants_serialize_camel_case() {
    let cases = vec![
        (SessionStatus::Queued, "queued"),
        (SessionStatus::Running, "running"),
        (SessionStatus::Idle, "idle"),
        (SessionStatus::Paused, "paused"),
        (SessionStatus::Completed, "completed"),
        (SessionStatus::Failed, "failed"),
        (SessionStatus::Archived, "archived"),
    ];

    for (status, expected_str) in cases {
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            json, expected_str,
            "SessionStatus::{status:?} should serialize to \"{expected_str}\""
        );
    }
}

#[test]
fn usage_serializes_snake_case() {
    let usage =
        Usage { input_tokens: 100, output_tokens: 50, total_tokens: 150, cost_usd: Some(0.001) };
    let json = serde_json::to_value(&usage).unwrap();

    assert_eq!(json["input_tokens"], 100);
    assert_eq!(json["output_tokens"], 50);
    assert_eq!(json["total_tokens"], 150);
    assert_eq!(json["cost_usd"], 0.001);
}

#[test]
fn usage_without_cost_deserializes() {
    let wire = json!({
        "input_tokens": 200,
        "output_tokens": 100,
        "total_tokens": 300
    });
    let usage: Usage = serde_json::from_value(wire).unwrap();
    assert_eq!(usage.input_tokens, 200);
    assert_eq!(usage.output_tokens, 100);
    assert_eq!(usage.total_tokens, 300);
    assert_eq!(usage.cost_usd, None);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4. ListResponse deserialization (with/without next_cursor)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn list_response_with_next_cursor() {
    let wire = json!({
        "data": [
            {"id": "agt_1", "name": "Agent 1", "model": "gemini-2.5-flash", "version": 1, "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"},
            {"id": "agt_2", "name": "Agent 2", "model": "gpt-4.1", "version": 1, "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"}
        ],
        "next_cursor": "cursor_abc123",
        "has_more": true
    });

    let response: ListResponse<Agent> = serde_json::from_value(wire).unwrap();
    assert_eq!(response.data.len(), 2);
    assert_eq!(response.next_cursor, Some("cursor_abc123".to_string()));
    assert!(response.has_more);
}

#[test]
fn list_response_without_next_cursor() {
    let wire = json!({
        "data": [
            {"id": "ses_1", "agent_id": "agt_1", "status": "idle", "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"}
        ],
        "has_more": false
    });

    let response: ListResponse<Session> = serde_json::from_value(wire).unwrap();
    assert_eq!(response.data.len(), 1);
    assert_eq!(response.next_cursor, None);
    assert!(!response.has_more);
}

#[test]
fn list_response_empty_data() {
    let wire = json!({
        "data": [],
        "has_more": false
    });

    let response: ListResponse<Agent> = serde_json::from_value(wire).unwrap();
    assert!(response.data.is_empty());
    assert_eq!(response.next_cursor, None);
    assert!(!response.has_more);
}

#[test]
fn list_response_with_null_next_cursor() {
    let wire = json!({
        "data": [],
        "next_cursor": null,
        "has_more": false
    });

    let response: ListResponse<Agent> = serde_json::from_value(wire).unwrap();
    assert_eq!(response.next_cursor, None);
}

#[test]
fn list_response_serializes_snake_case() {
    let response = ListResponse {
        data: vec!["item1".to_string(), "item2".to_string()],
        next_cursor: Some("cur_next".to_string()),
        has_more: true,
    };
    let json = serde_json::to_value(&response).unwrap();

    assert_eq!(json["data"], json!(["item1", "item2"]));
    assert_eq!(json["next_cursor"], "cur_next");
    assert_eq!(json["has_more"], true);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 5. ToolConfig (builtin vs custom) round-trip
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn tool_config_builtin_wire_shape() {
    let tool = ToolConfig::builtin("web_search");
    let json = serde_json::to_value(&tool).unwrap();

    assert_eq!(json, json!({"type": "builtin", "name": "web_search"}));
}

#[test]
fn tool_config_custom_wire_shape_with_input_schema() {
    let schema = json!({
        "type": "object",
        "properties": {
            "query": {"type": "string"},
            "limit": {"type": "integer"}
        },
        "required": ["query"]
    });
    let tool = ToolConfig::custom("search_docs", "Search documentation", schema.clone());
    let json = serde_json::to_value(&tool).unwrap();

    assert_eq!(json["type"], "custom");
    assert_eq!(json["name"], "search_docs");
    assert_eq!(json["description"], "Search documentation");
    assert_eq!(json["input_schema"], schema);
}

#[test]
fn tool_config_custom_without_optional_fields_wire_shape() {
    let tool = ToolConfig::Custom { name: "noop".into(), description: None, input_schema: None };
    let json = serde_json::to_value(&tool).unwrap();

    // Only type and name are present
    assert_eq!(json, json!({"type": "custom", "name": "noop"}));
}

#[test]
fn tool_config_builtin_round_trip_from_wire() {
    let wire = r#"{"type":"builtin","name":"code_execution"}"#;
    let tool: ToolConfig = serde_json::from_str(wire).unwrap();
    let reserialized = serde_json::to_string(&tool).unwrap();
    let re_deserialized: ToolConfig = serde_json::from_str(&reserialized).unwrap();
    assert_eq!(tool, re_deserialized);
}

#[test]
fn tool_config_custom_round_trip_from_wire() {
    let wire = r#"{"type":"custom","name":"my_tool","description":"Does stuff","input_schema":{"type":"object"}}"#;
    let tool: ToolConfig = serde_json::from_str(wire).unwrap();
    let reserialized = serde_json::to_string(&tool).unwrap();
    let re_deserialized: ToolConfig = serde_json::from_str(&reserialized).unwrap();
    assert_eq!(tool, re_deserialized);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 6. PermissionPolicy / PermissionMode serialization
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn permission_mode_auto_approve_wire_format() {
    let policy = PermissionPolicy { mode: PermissionMode::AutoApprove };
    let json = serde_json::to_value(&policy).unwrap();
    assert_eq!(json, json!({"mode": "autoApprove"}));
}

#[test]
fn permission_mode_prompt_wire_format() {
    let policy = PermissionPolicy { mode: PermissionMode::Prompt };
    let json = serde_json::to_value(&policy).unwrap();
    assert_eq!(json, json!({"mode": "prompt"}));
}

#[test]
fn permission_mode_deny_wire_format() {
    let policy = PermissionPolicy { mode: PermissionMode::Deny };
    let json = serde_json::to_value(&policy).unwrap();
    assert_eq!(json, json!({"mode": "deny"}));
}

#[test]
fn permission_policy_round_trip_all_modes() {
    for mode in [PermissionMode::AutoApprove, PermissionMode::Prompt, PermissionMode::Deny] {
        let policy = PermissionPolicy { mode };
        let wire = serde_json::to_string(&policy).unwrap();
        let parsed: PermissionPolicy = serde_json::from_str(&wire).unwrap();
        assert_eq!(policy, parsed);
    }
}

#[test]
fn permission_policy_deserializes_from_api_wire() {
    let wire = r#"{"mode":"autoApprove"}"#;
    let policy: PermissionPolicy = serde_json::from_str(wire).unwrap();
    assert_eq!(policy.mode, PermissionMode::AutoApprove);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 7. Vault / Memory type serialization
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn create_vault_params_serializes_snake_case() {
    let params = CreateVaultParams {
        name: "User Creds".into(),
        description: Some("OAuth credentials".into()),
    };
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["name"], "User Creds");
    assert_eq!(json["description"], "OAuth credentials");
}

#[test]
fn create_vault_params_skips_none_description() {
    let params = CreateVaultParams { name: "Minimal".into(), description: None };
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["name"], "Minimal");
    assert!(json.get("description").is_none());
}

#[test]
fn vault_response_deserializes_from_api() {
    let wire = json!({
        "id": "vault_abc",
        "name": "My Vault",
        "description": "Stores creds",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
        "archived_at": null
    });

    let vault: Vault = serde_json::from_value(wire).unwrap();
    assert_eq!(vault.id, "vault_abc");
    assert_eq!(vault.name, "My Vault");
    assert_eq!(vault.description, Some("Stores creds".to_string()));
    assert_eq!(vault.archived_at, None);
}

#[test]
fn create_credential_static_bearer_wire_shape() {
    let params = CreateCredentialParams::static_bearer(
        "GitHub Token",
        "https://api.github.com",
        "ghp_xxxxx",
    );
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["name"], "GitHub Token");
    assert_eq!(json["url"], "https://api.github.com");
    assert_eq!(json["credential_type"], "static_bearer");
    assert_eq!(json["token"], "ghp_xxxxx");
    // OAuth fields not present
    assert!(json.get("access_token").is_none());
    assert!(json.get("expires_at").is_none());
    assert!(json.get("refresh_token").is_none());
}

#[test]
fn create_credential_mcp_oauth_wire_shape() {
    let params = CreateCredentialParams::mcp_oauth(
        "Slack OAuth",
        "https://slack.com/api",
        "xoxb-token",
        "2026-12-31T23:59:59Z",
        Some("refresh_xyz".into()),
    );
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["name"], "Slack OAuth");
    assert_eq!(json["url"], "https://slack.com/api");
    assert_eq!(json["credential_type"], "mcp_oauth");
    assert_eq!(json["access_token"], "xoxb-token");
    assert_eq!(json["expires_at"], "2026-12-31T23:59:59Z");
    assert_eq!(json["refresh_token"], "refresh_xyz");
    // Static bearer field not present
    assert!(json.get("token").is_none());
}

#[test]
fn credential_response_deserializes_from_api() {
    let wire = json!({
        "id": "cred_001",
        "name": "My Token",
        "url": "https://api.example.com",
        "credential_type": "static_bearer",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    });

    let cred: Credential = serde_json::from_value(wire).unwrap();
    assert_eq!(cred.id, "cred_001");
    assert_eq!(cred.name, "My Token");
    assert_eq!(cred.credential_type, "static_bearer");
}

#[test]
fn update_credential_params_skips_none_fields() {
    let params = UpdateCredentialParams { token: Some("new_token".into()), ..Default::default() };
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["token"], "new_token");
    assert!(json.get("access_token").is_none());
    assert!(json.get("expires_at").is_none());
    assert!(json.get("refresh_token").is_none());
}

#[test]
fn create_memory_store_params_wire_shape() {
    let params = CreateMemoryStoreParams {
        name: "Context Store".into(),
        description: Some("Long-term memory".into()),
    };
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["name"], "Context Store");
    assert_eq!(json["description"], "Long-term memory");
}

#[test]
fn create_memory_store_params_skips_none() {
    let params = CreateMemoryStoreParams { name: "Basic".into(), description: None };
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["name"], "Basic");
    assert!(json.get("description").is_none());
}

#[test]
fn memory_store_response_deserializes_from_api() {
    let wire = json!({
        "id": "ms_001",
        "name": "Main Store",
        "description": "Primary memory",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    });

    let store: MemoryStore = serde_json::from_value(wire).unwrap();
    assert_eq!(store.id, "ms_001");
    assert_eq!(store.name, "Main Store");
    assert_eq!(store.description, Some("Primary memory".to_string()));
}

#[test]
fn create_memory_params_wire_shape() {
    let params = CreateMemoryParams {
        content: "The user prefers dark mode.".into(),
        metadata: Some(HashMap::from([("source".into(), "chat".into())])),
    };
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["content"], "The user prefers dark mode.");
    assert_eq!(json["metadata"]["source"], "chat");
}

#[test]
fn create_memory_params_skips_none_metadata() {
    let params = CreateMemoryParams { content: "Simple memory.".into(), metadata: None };
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["content"], "Simple memory.");
    assert!(json.get("metadata").is_none());
}

#[test]
fn memory_response_deserializes_from_api() {
    let wire = json!({
        "id": "mem_001",
        "store_id": "ms_001",
        "content": "User prefers concise responses",
        "metadata": {"session": "ses_abc"},
        "version": 2,
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-02T00:00:00Z"
    });

    let memory: Memory = serde_json::from_value(wire).unwrap();
    assert_eq!(memory.id, "mem_001");
    assert_eq!(memory.store_id, "ms_001");
    assert_eq!(memory.content, "User prefers concise responses");
    assert_eq!(memory.version, 2);
    assert_eq!(memory.metadata, Some(HashMap::from([("session".into(), "ses_abc".into())])));
}

#[test]
fn update_memory_params_wire_shape() {
    let params = UpdateMemoryParams {
        content: "Updated preference".into(),
        metadata: Some(HashMap::from([("updated_by".into(), "user".into())])),
    };
    let json = serde_json::to_value(&params).unwrap();

    assert_eq!(json["content"], "Updated preference");
    assert_eq!(json["metadata"]["updated_by"], "user");
}

#[test]
fn memory_version_deserializes_from_api() {
    let wire = json!({
        "version": 3,
        "content": "Third revision of memory",
        "created_at": "2026-01-03T00:00:00Z"
    });

    let version: MemoryVersion = serde_json::from_value(wire).unwrap();
    assert_eq!(version.version, 3);
    assert_eq!(version.content, "Third revision of memory");
}

// ═══════════════════════════════════════════════════════════════════════════════
// 8. ModelRef untagged deserialization (shorthand + structured + compatible)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn model_ref_shorthand_from_plain_string() {
    let wire = r#""gemini-2.5-flash""#;
    let model_ref: ModelRef = serde_json::from_str(wire).unwrap();
    assert_eq!(model_ref, ModelRef::Shorthand("gemini-2.5-flash".to_string()));
}

#[test]
fn model_ref_structured_from_object() {
    let wire = json!({"provider": "openai", "model": "gpt-4.1"});
    let model_ref: ModelRef = serde_json::from_value(wire).unwrap();
    assert_eq!(
        model_ref,
        ModelRef::Structured {
            provider: Provider::Openai,
            model: ModelConfig::Name("gpt-4.1".to_string()),
            speed: None,
        }
    );
}

#[test]
fn model_ref_structured_with_speed_from_object() {
    let wire = json!({"provider": "gemini", "model": "gemini-2.5-flash", "speed": "fast"});
    let model_ref: ModelRef = serde_json::from_value(wire).unwrap();
    assert_eq!(
        model_ref,
        ModelRef::Structured {
            provider: Provider::Gemini,
            model: ModelConfig::Name("gemini-2.5-flash".to_string()),
            speed: Some("fast".to_string()),
        }
    );
}

#[test]
fn model_ref_compatible_from_object() {
    let wire = json!({
        "provider": "openaiCompatible",
        "model": {
            "model": "deepseek-chat",
            "base_url": "https://api.deepseek.com"
        }
    });
    let model_ref: ModelRef = serde_json::from_value(wire).unwrap();
    assert_eq!(
        model_ref,
        ModelRef::Structured {
            provider: Provider::OpenaiCompatible,
            model: ModelConfig::Compatible {
                model: "deepseek-chat".to_string(),
                base_url: "https://api.deepseek.com".to_string(),
                api_key: None,
            },
            speed: None,
        }
    );
}

#[test]
fn model_ref_compatible_with_api_key_from_object() {
    let wire = json!({
        "provider": "openaiCompatible",
        "model": {
            "model": "mixtral-8x7b",
            "base_url": "https://api.together.xyz",
            "api_key": "sk-together"
        }
    });
    let model_ref: ModelRef = serde_json::from_value(wire).unwrap();
    assert_eq!(
        model_ref,
        ModelRef::Structured {
            provider: Provider::OpenaiCompatible,
            model: ModelConfig::Compatible {
                model: "mixtral-8x7b".to_string(),
                base_url: "https://api.together.xyz".to_string(),
                api_key: Some("sk-together".to_string()),
            },
            speed: None,
        }
    );
}

#[test]
fn model_ref_shorthand_serializes_to_string() {
    let model_ref = ModelRef::from("claude-sonnet-4-6");
    let json = serde_json::to_value(&model_ref).unwrap();
    assert_eq!(json, "claude-sonnet-4-6");
}

#[test]
fn model_ref_structured_serializes_provider_camel_case() {
    let model_ref = ModelRef::structured(Provider::OpenaiCompatible, "model-x");
    let json = serde_json::to_value(&model_ref).unwrap();
    assert_eq!(json["provider"], "openaiCompatible");
}

#[test]
fn model_ref_untagged_round_trip_shorthand() {
    let original = ModelRef::from("gpt-4.1");
    let wire = serde_json::to_string(&original).unwrap();
    let parsed: ModelRef = serde_json::from_str(&wire).unwrap();
    assert_eq!(original, parsed);
}

#[test]
fn model_ref_untagged_round_trip_structured() {
    let original = ModelRef::structured(Provider::Anthropic, "claude-sonnet-4-6");
    let wire = serde_json::to_string(&original).unwrap();
    let parsed: ModelRef = serde_json::from_str(&wire).unwrap();
    assert_eq!(original, parsed);
}

#[test]
fn model_ref_untagged_round_trip_compatible() {
    let original =
        ModelRef::compatible_with_key("deepseek-chat", "https://api.deepseek.com", "sk-key");
    let wire = serde_json::to_string(&original).unwrap();
    let parsed: ModelRef = serde_json::from_str(&wire).unwrap();
    assert_eq!(original, parsed);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 9. SessionEvent unknown type → Unknown variant
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn session_event_unknown_future_type() {
    let wire = json!({
        "type": "agent.thinking",
        "seq": 42,
        "thoughts": "reasoning about the problem"
    });
    let event: SessionEvent = serde_json::from_value(wire).unwrap();
    assert!(matches!(event, SessionEvent::Unknown));
}

#[test]
fn session_event_unknown_completely_novel_namespace() {
    let wire = json!({
        "type": "system.maintenance",
        "seq": 1,
        "message": "Server restarting"
    });
    let event: SessionEvent = serde_json::from_value(wire).unwrap();
    assert!(matches!(event, SessionEvent::Unknown));
}

#[test]
fn session_event_unknown_empty_type_string() {
    let wire = json!({
        "type": "",
        "seq": 0
    });
    let event: SessionEvent = serde_json::from_value(wire).unwrap();
    assert!(matches!(event, SessionEvent::Unknown));
}
