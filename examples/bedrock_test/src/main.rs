use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use adk_rust::futures::StreamExt;
use adk_tool::tool;
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Deserialize, JsonSchema)]
struct ArchReviewArgs {
    /// Description of the system to review
    system: String,
    /// Expected requests per second
    rps: u32,
}

/// Review a cloud architecture and return component recommendations with cost estimate.
#[tool]
async fn review_architecture(args: ArchReviewArgs) -> adk_tool::Result<serde_json::Value> {
    let tier = if args.rps > 10000 {
        "enterprise"
    } else if args.rps > 1000 {
        "growth"
    } else {
        "starter"
    };
    let mut components = vec![
        serde_json::json!({"service": "API Gateway", "purpose": "Request routing & throttling"}),
        serde_json::json!({"service": "Lambda / ECS", "purpose": "Compute layer"}),
        serde_json::json!({"service": "DynamoDB", "purpose": "Low-latency data store"}),
    ];
    if args.rps > 1000 {
        components.push(serde_json::json!({"service": "CloudFront", "purpose": "CDN edge caching"}));
        components.push(serde_json::json!({"service": "SQS", "purpose": "Async message queue"}));
        components.push(
            serde_json::json!({"service": "ElastiCache", "purpose": "In-memory caching layer"}),
        );
    }
    Ok(serde_json::json!({
        "system": args.system,
        "tier": tier,
        "expected_rps": args.rps,
        "components": components,
        "estimated_monthly_cost": match tier {
            "enterprise" => "$5,000-$20,000",
            "growth" => "$500-$5,000",
            _ => "$50-$500",
        }
    }))
}

#[derive(Deserialize, JsonSchema)]
struct ThreatArgs {
    /// Architecture components to analyze for threats
    components: Vec<String>,
}

/// Perform a threat model analysis on the given architecture components.
#[tool]
async fn analyze_threats(args: ThreatArgs) -> adk_tool::Result<serde_json::Value> {
    let threats: Vec<_> = args
        .components
        .iter()
        .map(|c| {
            let (threat, mitigation) = match c.to_lowercase() {
                s if s.contains("api") => {
                    ("Unauthorized access / DDoS", "WAF + rate limiting + API keys")
                }
                s if s.contains("lambda") || s.contains("ecs") => {
                    ("Code injection", "IAM least-privilege + VPC isolation")
                }
                s if s.contains("dynamo") || s.contains("rds") => {
                    ("Data exfiltration", "Encryption at rest + VPC endpoints")
                }
                s if s.contains("sqs") => {
                    ("Message tampering", "SSE-SQS encryption + dead-letter queues")
                }
                _ => ("Misconfiguration", "Security review + AWS Config rules"),
            };
            serde_json::json!({
                "component": c,
                "threat": threat,
                "mitigation": mitigation,
            })
        })
        .collect();
    Ok(serde_json::json!({
        "components_analyzed": args.components.len(),
        "threats": threats,
    }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let region = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".into());
    let model_id = std::env::var("BEDROCK_MODEL_ID")
        .unwrap_or_else(|_| "us.anthropic.claude-sonnet-4-6".into());

    let config = BedrockConfig::new(&region, &model_id);
    let model = Arc::new(BedrockClient::new(config).await?);

    let agent = Arc::new(
        LlmAgentBuilder::new("cloud_architect")
            .instruction(
                "You are a cloud solutions architect powered by Amazon Bedrock.\n\
                 Use review_architecture to create infrastructure recommendations,\n\
                 then use analyze_threats to assess security risks of the proposed components.\n\
                 Present a clear architecture overview followed by the security analysis.",
            )
            .model(model)
            .tool(Arc::new(ReviewArchitecture))
            .tool(Arc::new(AnalyzeThreats))
            .build()?,
    );

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "playground".into(),
            user_id: "user".into(),
            session_id: Some("s1".into()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::new(RunnerConfig {
        app_name: "playground".into(),
        agent,
        session_service: sessions,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
    })?;

    println!("🏗️  Amazon Bedrock — {} ({})\n", model_id, region);

    let message = Content::new("user").with_text(
        "Design a high-availability e-commerce API handling 5000 requests/second, \
         then analyze the security threats for the proposed architecture.",
    );

    let mut stream = runner.run(adk_rust::UserId::new("user")?, adk_rust::SessionId::new("s1")?, message).await?;
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Some(text) = part.text() {
                    print!("{text}");
                }
            }
        }
    }
    println!();
    Ok(())
}
