//! Item 5: the Responses client records `provider_error_code` from `code`, then `type`, then
//! `status`.

use std::sync::Arc;

use adk_model::RetryConfig;
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use serde_json::{Value, json};

use crate::common::{Mode, Report, Verdict, agent_turn, check, describe, fail, provider_code};
use crate::upstream::{Reply, Upstream};

pub async fn run(report: &mut Report) {
    report.record_result(
        "responses-error-type",
        5,
        Mode::Scripted,
        case(
            json!({"error": {"message": "Invalid value for 'input'.", "type": "invalid_request_error",
                             "param": "input", "code": null}}),
            "invalid_request_error",
        )
        .await,
    );
    report.record_result(
        "responses-error-status",
        5,
        Mode::Scripted,
        case(
            json!({"error": {"message": "Request contains an invalid argument.", "code": 400,
                             "status": "INVALID_ARGUMENT"}}),
            "INVALID_ARGUMENT",
        )
        .await,
    );
}

async fn case(body: Value, expected: &str) -> anyhow::Result<Verdict> {
    let upstream = Upstream::start().await?;
    upstream.push(Reply::Json(400, body));
    let client = OpenAIResponsesClient::new(
        OpenAIResponsesConfig::new("test-key", "gpt-5.6-terra")
            .with_base_url(format!("{}/v1", upstream.base)),
    )?
    .with_retry_config(RetryConfig::disabled());
    let turn = agent_turn(Arc::new(client), None, "Say hi.").await?;
    let Some(error) = turn.error else {
        return Ok(fail("the turn succeeded against an HTTP 400"));
    };
    let code = provider_code(&error);
    Ok(check(
        code.as_deref() == Some(expected),
        format!("provider_error_code {code:?} ; {}", describe(&error)),
    ))
}
