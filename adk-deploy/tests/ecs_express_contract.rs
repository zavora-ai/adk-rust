//! Contract tests for the Amazon ECS Express Mode target.
//!
//! Request bodies are pinned against the shapes in the Amazon ECS API reference
//! (`CreateExpressGatewayService`, `UpdateExpressGatewayService`); responses are recorded
//! fixtures transcribed from its response syntax. Every request is checked for the JSON 1.1
//! protocol headers and a Signature Version 4 signature recomputed over what was received.

#![cfg(feature = "aws")]

use std::path::Path;
use std::time::{Duration, SystemTime};

use adk_deploy::DeploymentManifest;
use adk_deploy::aws::{EcsExpressClient, EcsExpressTarget, EcsExpressTargetConfig};
use adk_deploy::targets::{DeployRequest, DeployTarget, DeploymentOutcome, DeploymentStatus};
use aws_credential_types::Credentials;
use aws_credential_types::provider::SharedCredentialsProvider;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const IMAGE: &str = "123456789012.dkr.ecr.us-east-1.amazonaws.com/support-desk:1.4.0";
const SERVICE_ARN: &str = "arn:aws:ecs:us-east-1:123456789012:service/default/support-desk";
const ACCESS_KEY: &str = "AKIDEXAMPLE";
const SECRET_KEY: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name))
        .unwrap()
}

fn manifest() -> DeploymentManifest {
    toml::from_str(&fixture("ecs-express.toml")).unwrap()
}

fn credentials() -> SharedCredentialsProvider {
    SharedCredentialsProvider::new(Credentials::new(ACCESS_KEY, SECRET_KEY, None, None, "test"))
}

fn target(server: &MockServer, manifest: &DeploymentManifest) -> EcsExpressTarget {
    let config: EcsExpressTargetConfig = manifest.target_config(EcsExpressTarget::NAME).unwrap();
    let client = EcsExpressClient::with_credentials("us-east-1", credentials())
        .unwrap()
        .with_endpoint(&server.uri())
        .unwrap();
    EcsExpressTarget::with_client(client, config)
        .with_polling(Duration::from_secs(5), Duration::from_millis(10))
}

fn operation(name: &str) -> wiremock::matchers::HeaderExactMatcher {
    header("x-amz-target", format!("AmazonEC2ContainerServiceV20141113.{name}").as_str())
}

fn respond(status: u16, body: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_raw(body.to_string(), "application/x-amz-json-1.1")
}

/// Re-signs the received request with the same credentials and time and compares signatures,
/// so the body and headers sent are the ones that were signed.
fn assert_signed(request: &Request) {
    let header = |name: &str| request.headers.get(name).unwrap().to_str().unwrap().to_string();
    assert_eq!(header("content-type"), "application/x-amz-json-1.1");
    let authorization = header("authorization");
    let date = header("x-amz-date");
    assert!(
        authorization.starts_with(&format!(
            "AWS4-HMAC-SHA256 Credential={ACCESS_KEY}/{}/us-east-1/ecs/aws4_request, SignedHeaders=",
            &date[..8]
        )),
        "{authorization}"
    );
    let signed_headers =
        authorization.split("SignedHeaders=").nth(1).unwrap().split(',').next().unwrap();
    for name in ["content-type", "host", "x-amz-date", "x-amz-target"] {
        assert!(
            signed_headers.split(';').any(|signed| signed == name),
            "{name} not signed: {signed_headers}"
        );
    }

    let time = chrono::NaiveDateTime::parse_from_str(&date, "%Y%m%dT%H%M%SZ").unwrap().and_utc();
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(time.timestamp() as u64);
    let identity = Credentials::new(ACCESS_KEY, SECRET_KEY, None, None, "test").into();
    let params = aws_sigv4::sign::v4::SigningParams::builder()
        .identity(&identity)
        .region("us-east-1")
        .name("ecs")
        .time(time)
        .settings(aws_sigv4::http_request::SigningSettings::default())
        .build()
        .unwrap()
        .into();
    let target = header("x-amz-target");
    let headers =
        [("content-type", "application/x-amz-json-1.1"), ("x-amz-target", target.as_str())];
    // wiremock reports the URL with a placeholder host; the signed host is the one received.
    let url = format!("http://{}{}", header("host"), request.url.path());
    let signable = aws_sigv4::http_request::SignableRequest::new(
        "POST",
        url.as_str(),
        headers.iter().copied(),
        aws_sigv4::http_request::SignableBody::Bytes(&request.body),
    )
    .unwrap();
    let (_, signature) = aws_sigv4::http_request::sign(signable, &params).unwrap().into_parts();
    assert!(
        authorization.ends_with(&format!("Signature={signature}")),
        "signature mismatch: {authorization}"
    );
}

fn body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

fn expected_container() -> Value {
    json!({
        "image": IMAGE,
        "containerPort": 8080,
        "environment": [
            { "name": "ADK_AGENT_NAME", "value": "support-desk" },
            { "name": "ADK_AGENT_VERSION", "value": "1.4.0" },
            { "name": "ADK_ALLOWED_HOSTS", "value": "support.example.com,.ecs.us-east-1.on.aws" },
            { "name": "ADK_AUTH_MODE", "value": "bearer" },
            { "name": "ADK_AUTH_REQUIRED_SCOPES", "value": "agent:invoke" },
            { "name": "ADK_DEPLOY_TARGET", "value": "ecs-express" },
            { "name": "BIND_HOST", "value": "0.0.0.0" },
            { "name": "PORT", "value": "8080" },
            { "name": "RUST_LOG", "value": "info" },
        ],
        "secrets": [
            { "name": "ADK_API_TOKEN", "valueFrom": "/support-desk/ADK_API_TOKEN" },
            {
                "name": "ADK_SERVICE_SESSIONS_URL",
                "valueFrom": "arn:aws:secretsmanager:us-east-1:123456789012:secret:support-desk/database-url-GhIjKl",
            },
            {
                "name": "ANTHROPIC_API_KEY",
                "valueFrom": "arn:aws:secretsmanager:us-east-1:123456789012:secret:support-desk/anthropic-AbCdEf",
            },
        ],
        "awsLogsConfiguration": { "logGroup": "/adk/support-desk", "logStreamPrefix": "support-desk" },
    })
}

fn expected_create_body() -> Value {
    json!({
        "serviceName": "support-desk",
        "executionRoleArn": "arn:aws:iam::123456789012:role/ecsTaskExecutionRole",
        "infrastructureRoleArn": "arn:aws:iam::123456789012:role/ecsInfrastructureRoleForExpressServices",
        "healthCheckPath": "/api/health",
        "cpu": "512",
        "memory": "1024",
        "networkConfiguration": {
            "securityGroups": ["sg-0123456789abcdef0"],
            "subnets": ["subnet-0a1b2c3d", "subnet-4e5f6a7b"],
        },
        "primaryContainer": expected_container(),
        "scalingTarget": {
            "autoScalingMetric": "AVERAGE_CPU",
            "autoScalingTargetValue": 65,
            "minTaskCount": 2,
            "maxTaskCount": 6,
        },
        "tags": [
            { "key": "adk:agent", "value": "support-desk" },
            { "key": "adk:version", "value": "1.4.0" },
            { "key": "managed-by", "value": "adk-deploy" },
        ],
    })
}

#[tokio::test]
async fn plan_renders_the_create_request_in_the_reference_shape() {
    let server = MockServer::start().await;
    let manifest = manifest();
    let plan = target(&server, &manifest).plan(&DeployRequest::new(manifest, IMAGE)).unwrap();
    assert_eq!(plan.resources.len(), 1);
    assert_eq!(plan.resources[0].kind, "ecs:CreateExpressGatewayService");
    assert_eq!(plan.resources[0].name, "support-desk");
    assert_eq!(
        serde_json::from_str::<Value>(&plan.resources[0].body).unwrap(),
        expected_create_body()
    );
    assert!(plan.runtime.public);
    assert!(server.received_requests().await.unwrap().is_empty(), "planning contacts nothing");
}

#[tokio::test]
async fn deploy_creates_the_service_and_waits_for_its_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(operation("CreateExpressGatewayService"))
        .respond_with(respond(200, &fixture("ecs_express/create-response.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(operation("DescribeExpressGatewayService"))
        .respond_with(respond(200, &fixture("ecs_express/describe-provisioning.json")))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(operation("DescribeExpressGatewayService"))
        .respond_with(respond(200, &fixture("ecs_express/describe-active.json")))
        .with_priority(2)
        .mount(&server)
        .await;

    let manifest = manifest();
    let outcome =
        target(&server, &manifest).deploy(&DeployRequest::new(manifest, IMAGE)).await.unwrap();
    assert_eq!(
        outcome,
        DeploymentOutcome::new(
            "ecs-express",
            SERVICE_ARN,
            Some("https://support-desk-a1b2c3.ecs.us-east-1.on.aws".to_string()),
            DeploymentStatus::Ready,
        )
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4, "create, two provisioning polls, one active poll");
    for request in &requests {
        assert_signed(request);
    }
    assert_eq!(body(&requests[0]), expected_create_body());
    assert_eq!(body(&requests[1]), json!({ "serviceArn": SERVICE_ARN }));
}

#[tokio::test]
async fn deploy_updates_the_service_named_by_service_arn() {
    let server = MockServer::start().await;
    Mock::given(operation("UpdateExpressGatewayService"))
        .respond_with(respond(200, &fixture("ecs_express/create-response.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(operation("DescribeExpressGatewayService"))
        .respond_with(respond(200, &fixture("ecs_express/describe-active.json")))
        .mount(&server)
        .await;

    let mut manifest = manifest();
    manifest.targets.get_mut("ecs-express").unwrap()["serviceArn"] = json!(SERVICE_ARN);
    let outcome =
        target(&server, &manifest).deploy(&DeployRequest::new(manifest, IMAGE)).await.unwrap();
    assert_eq!(outcome.status, DeploymentStatus::Ready);

    let requests = server.received_requests().await.unwrap();
    assert_signed(&requests[0]);
    let mut expected = expected_create_body();
    let expected = expected.as_object_mut().unwrap();
    for removed in ["serviceName", "infrastructureRoleArn", "tags"] {
        expected.remove(removed);
    }
    expected.insert("serviceArn".to_string(), json!(SERVICE_ARN));
    assert_eq!(body(&requests[0]), Value::Object(expected.clone()));
}

#[tokio::test]
async fn status_maps_the_service_state() {
    let server = MockServer::start().await;
    Mock::given(operation("DescribeExpressGatewayService"))
        .respond_with(respond(200, &fixture("ecs_express/describe-provisioning.json")))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(operation("DescribeExpressGatewayService"))
        .respond_with(respond(200, &fixture("ecs_express/describe-active.json")))
        .up_to_n_times(1)
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(operation("DescribeExpressGatewayService"))
        .respond_with(respond(400, &fixture("ecs_express/not-found.json")))
        .with_priority(3)
        .mount(&server)
        .await;

    let target = target(&server, &manifest());
    assert_eq!(
        target.status(SERVICE_ARN).await.unwrap(),
        DeploymentStatus::Pending { detail: "waiting for the load balancer endpoint".to_string() }
    );
    assert_eq!(target.status(SERVICE_ARN).await.unwrap(), DeploymentStatus::Ready);
    assert_eq!(target.status(SERVICE_ARN).await.unwrap(), DeploymentStatus::NotFound);
}

#[tokio::test]
async fn destroy_deletes_the_service() {
    let server = MockServer::start().await;
    Mock::given(operation("DeleteExpressGatewayService"))
        .respond_with(respond(200, &fixture("ecs_express/create-response.json")))
        .expect(1)
        .mount(&server)
        .await;
    target(&server, &manifest()).destroy(SERVICE_ARN).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_signed(&requests[0]);
    assert_eq!(body(&requests[0]), json!({ "serviceArn": SERVICE_ARN }));
}

#[tokio::test]
async fn api_errors_carry_the_exception_type_and_message() {
    let server = MockServer::start().await;
    Mock::given(operation("CreateExpressGatewayService"))
        .respond_with(respond(400, &fixture("ecs_express/access-denied.json")))
        .mount(&server)
        .await;
    let manifest = manifest();
    let error =
        target(&server, &manifest).deploy(&DeployRequest::new(manifest, IMAGE)).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "control-plane request failed: ECS CreateExpressGatewayService failed: AccessDeniedException: \
         User: arn:aws:iam::123456789012:user/ci is not authorized to perform: iam:PassRole on \
         resource: arn:aws:iam::123456789012:role/ecsTaskExecutionRole"
    );
}

#[tokio::test]
async fn incomplete_settings_are_refused_before_any_request() {
    let server = MockServer::start().await;

    let mut unmapped = manifest();
    let table = unmapped.targets.get_mut("ecs-express").unwrap().as_object_mut().unwrap();
    table.remove("ssmParameterPrefix");
    let error =
        target(&server, &unmapped).plan(&DeployRequest::new(unmapped.clone(), IMAGE)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("needs a Secrets Manager ARN or Parameter Store parameter for ADK_API_TOKEN"),
        "{error}"
    );

    let mut no_role = manifest();
    no_role
        .targets
        .get_mut("ecs-express")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("infrastructureRoleArn");
    let error =
        target(&server, &no_role).plan(&DeployRequest::new(no_role.clone(), IMAGE)).unwrap_err();
    assert!(error.to_string().contains("needs infrastructureRoleArn"), "{error}");

    let mut no_auth = manifest();
    no_auth.auth = None;
    let error =
        target(&server, &no_auth).plan(&DeployRequest::new(no_auth.clone(), IMAGE)).unwrap_err();
    assert!(
        error.to_string().contains("exposes the agent publicly, so callers must authenticate"),
        "{error}"
    );

    let mut sqlite = manifest();
    sqlite.services[0].kind = adk_deploy::ServiceKind::Sqlite;
    sqlite.scaling.max_instances = 1;
    sqlite.scaling.min_instances = 1;
    let error =
        target(&server, &sqlite).plan(&DeployRequest::new(sqlite.clone(), IMAGE)).unwrap_err();
    assert!(
        error.to_string().contains("ECS Express Mode tasks have no persistent volume"),
        "{error}"
    );

    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn endpoints_must_be_https_or_loopback() {
    let client = EcsExpressClient::with_credentials("us-east-1", credentials()).unwrap();
    assert!(client.clone().with_endpoint("http://ecs.example.com").is_err());
    assert!(client.clone().with_endpoint("http://127.0.0.1:9000").is_ok());
    assert!(client.with_endpoint("https://ecs-fips.us-east-1.amazonaws.com").is_ok());
}
