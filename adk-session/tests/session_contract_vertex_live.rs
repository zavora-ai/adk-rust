#![cfg(feature = "vertex-session")]

mod common;

use adk_core::{Content, FunctionResponseData, Part};
use adk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, SessionService, VertexAiSessionConfig,
    VertexAiSessionService,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

const ENV_PROJECT_ID_KEYS: [&str; 2] = ["GOOGLE_PROJECT_ID", "GOOGLE_CLOUD_PROJECT"];
const ENV_LOCATION_KEYS: [&str; 2] = ["GOOGLE_CLOUD_LOCATION", "GOOGLE_VERTEX_LOCATION"];
const ENV_APP_NAME_KEYS: [&str; 2] = ["GOOGLE_VERTEX_APP_NAME", "ADK_VERTEX_SESSION_APP_NAME"];
const ENV_OTHER_APP_NAME_KEYS: [&str; 2] =
    ["GOOGLE_VERTEX_OTHER_APP_NAME", "ADK_VERTEX_SESSION_OTHER_APP_NAME"];

fn required_env_any(keys: &[&str]) -> String {
    for key in keys {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }

    panic!("one of [{}] is required for live Vertex session contract test", keys.join(", "))
}

#[tokio::test]
#[ignore = "requires live Vertex Session Service resources + ADC; run with --ignored"]
async fn test_vertex_service_live_contract() {
    let project_id = required_env_any(&ENV_PROJECT_ID_KEYS);
    let location = required_env_any(&ENV_LOCATION_KEYS);
    let app_name = required_env_any(&ENV_APP_NAME_KEYS);
    let other_app_name = required_env_any(&ENV_OTHER_APP_NAME_KEYS);

    let service =
        VertexAiSessionService::new_with_adc(VertexAiSessionConfig::new(project_id, location))
            .expect("build vertex session service");

    let run_id = Uuid::new_v4().simple().to_string();
    let user_1 = format!("adk-rust-live-u1-{run_id}");
    let user_2 = format!("adk-rust-live-u2-{run_id}");

    common::session_contract::assert_session_contract_with_users(
        &service,
        &app_name,
        &other_app_name,
        &user_1,
        &user_2,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires live Vertex Session Service resources + ADC; run with --ignored"]
async fn test_vertex_live_from_env_ttl_multi_turn_round_trip() {
    let config = VertexAiSessionConfig::from_env()
        .expect("GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION, and GOOGLE_CLOUD_AGENT_ENGINE_ID are required")
        .with_ttl(Duration::from_secs(86_400));
    let service =
        VertexAiSessionService::new_with_adc(config).expect("build vertex session service");

    let run_id = Uuid::new_v4().simple().to_string();
    let app_name = format!("adk-rust-live-ttl-{run_id}");
    let user_id = format!("adk-rust-live-ttl-user-{run_id}");
    let session_id = format!("live-ttl-{run_id}");

    service
        .create(CreateRequest {
            app_name: app_name.clone(),
            user_id: user_id.clone(),
            session_id: Some(session_id.clone()),
            state: HashMap::new(),
        })
        .await
        .expect("create session with ttl");

    let verification: Result<(), String> = async {
        let mut user_turn = Event::new("inv-live-ttl");
        user_turn.author = "user".to_string();
        user_turn.llm_response.content = Some(Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "what is the weather in Paris?".to_string() }],
        });

        let mut tool_call_turn = Event::new("inv-live-ttl");
        tool_call_turn.author = "model".to_string();
        tool_call_turn.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "get_weather".to_string(),
                args: json!({ "city": "Paris" }),
                id: None,
                thought_signature: None,
            }],
        });

        let mut tool_response_turn = Event::new("inv-live-ttl");
        tool_response_turn.author = "tool".to_string();
        tool_response_turn.llm_response.content = Some(Content {
            role: "tool".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData {
                    name: "get_weather".to_string(),
                    response: json!({ "tempC": 21, "condition": "sunny" }),
                    inline_data: vec![],
                    file_data: vec![],
                },
                id: None,
                annotations: None,
            }],
        });

        let mut answer_turn = Event::new("inv-live-ttl");
        answer_turn.author = "model".to_string();
        answer_turn.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![
                Part::Text { text: "It is 21°C and sunny in Paris.".to_string() },
                Part::InlineData {
                    mime_type: "image/png".to_string(),
                    data: vec![137, 80, 78, 71, 0, 255],
                    uri: None,
                    annotations: None,
                },
            ],
        });

        let turns = vec![user_turn, tool_call_turn, tool_response_turn, answer_turn];
        let expected = turns
            .iter()
            .map(|event| serde_json::to_value(event).map_err(|error| error.to_string()))
            .collect::<Result<Vec<Value>, String>>()?;
        for turn in turns {
            service
                .append_event(&session_id, turn)
                .await
                .map_err(|error| format!("append turn: {error}"))?;
        }

        let fetched = service
            .get(GetRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: session_id.clone(),
                num_recent_events: None,
                after: None,
            })
            .await
            .map_err(|error| format!("get session: {error}"))?;
        let restored = fetched
            .events()
            .all()
            .iter()
            .map(|event| serde_json::to_value(event).map_err(|error| error.to_string()))
            .collect::<Result<Vec<Value>, String>>()?;
        if restored != expected {
            return Err("reconstructed conversation does not equal the appended events".to_string());
        }
        Ok(())
    }
    .await;

    let cleanup = service
        .delete(DeleteRequest {
            app_name: app_name.clone(),
            user_id: user_id.clone(),
            session_id: session_id.clone(),
        })
        .await;
    match (verification, cleanup) {
        (Err(verification), Err(cleanup)) => {
            panic!("live ttl round trip failed: {verification}; cleanup also failed: {cleanup}")
        }
        (Err(verification), Ok(())) => panic!("live ttl round trip failed: {verification}"),
        (Ok(()), Err(cleanup)) => panic!("live ttl round trip cleanup failed: {cleanup}"),
        (Ok(()), Ok(())) => {}
    }
}
