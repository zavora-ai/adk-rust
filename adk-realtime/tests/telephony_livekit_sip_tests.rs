//! LiveKit SIP, against LiveKit's server API: the Twirp requests (protobuf bodies decoded with
//! `livekit-protocol`, tokens verified with the API secret) served by wiremock, signed
//! `participant_joined` webhooks, and a simulated call in which a scripted model stands in for
//! any realtime provider in the room and asks to transfer the caller through the governed path.

#![cfg(all(feature = "livekit-sip", feature = "scripted"))]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    Content, DeclarativePolicy, Event, GovernanceControl, Part, RunConfig, Tool,
    ToolConfirmationDecision, ToolConfirmationHandler, ToolConfirmationRequest,
};
use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply};
use adk_realtime::telephony::livekit::{
    LiveKitSip, LiveKitWebhookEvent, LiveKitWebhookVerifier, PARTICIPANT_IDENTITY_ATTRIBUTE,
    ROOM_ATTRIBUTE,
};
use adk_realtime::telephony::{
    CallAnswerer, CallControl, CallTarget, DialRequest, IncomingCall, OutboundDialer,
    PlaceCallTool, TransferCallTool, hang_up_on_freeze,
};
use adk_realtime::{RealtimeAgent, RealtimeConfig, RealtimeError};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use base64::Engine;
use futures::StreamExt;
use livekit_api::access_token::{AccessToken, Claims, TokenVerifier};
use livekit_protocol as proto;
use prost::Message;
use serde_json::{Value, json};
use sha2::Digest;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const API_KEY: &str = "APItest";
const API_SECRET: &str = "a-secret-long-enough-for-hs256-signing";
const ROOM: &str = "call-_+14155550123_x7Kq";
const CALLER: &str = "sip_+14155550123";

const CREATE_PARTICIPANT: &str = "/twirp/livekit.SIP/CreateSIPParticipant";
const TRANSFER: &str = "/twirp/livekit.SIP/TransferSIPParticipant";
const REMOVE: &str = "/twirp/livekit.RoomService/RemoveParticipant";
const INBOUND_TRUNK: &str = "/twirp/livekit.SIP/CreateSIPInboundTrunk";
const DISPATCH_RULE: &str = "/twirp/livekit.SIP/CreateSIPDispatchRule";

fn livekit(server: &MockServer) -> LiveKitSip {
    LiveKitSip::new(&server.uri(), API_KEY, API_SECRET)
}

/// Answers `path` with `response`, protobuf-encoded as Twirp does.
async fn twirp(server: &MockServer, route: &str, response: impl Message + 'static) {
    Mock::given(method("POST"))
        .and(path(route))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/protobuf")
                .set_body_bytes(response.encode_to_vec()),
        )
        .mount(server)
        .await;
}

async fn requests_to(server: &MockServer, route: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == route)
        .collect()
}

/// The request's bearer token, verified with the API secret.
fn claims(request: &Request) -> Claims {
    let header = request.headers.get("authorization").unwrap().to_str().unwrap();
    let token = header.strip_prefix("Bearer ").expect("a bearer token");
    TokenVerifier::with_api_key(API_KEY, API_SECRET).verify(token).unwrap()
}

// ===== Server API =====

#[tokio::test]
async fn dialling_creates_a_sip_participant_and_waits_for_the_answer() {
    let server = MockServer::start().await;
    twirp(
        &server,
        CREATE_PARTICIPANT,
        proto::SipParticipantInfo {
            participant_id: "PA_1".into(),
            participant_identity: "sip_+14155550199".into(),
            room_name: "incident-4101".into(),
            sip_call_id: "SCL_1".into(),
        },
    )
    .await;
    let dialer = livekit(&server).dialer("ST_outbound").with_room("incident-4101");

    let call = dialer
        .dial(
            &DialRequest::new(CallTarget::phone("+14155550199").unwrap())
                .with_attribute("dtmf", "1234#"),
        )
        .await
        .unwrap();

    assert_eq!((call.call_id(), call.provider()), ("SCL_1", "livekit"));
    let requests = requests_to(&server, CREATE_PARTICIPANT).await;
    let request = proto::CreateSipParticipantRequest::decode(&requests[0].body[..]).unwrap();
    assert_eq!(request.sip_trunk_id, "ST_outbound");
    assert_eq!(request.sip_call_to, "+14155550199");
    assert_eq!(request.room_name, "incident-4101");
    assert_eq!(request.participant_identity, "sip_+14155550199");
    assert_eq!(request.dtmf, "1234#");
    assert!(request.wait_until_answered);
    assert!(claims(&requests[0]).sip.call, "dialling needs the sip.call grant");
}

#[tokio::test]
async fn a_sip_target_or_a_missing_room_is_refused_before_any_request() {
    let server = MockServer::start().await;
    let dialer = livekit(&server).dialer("ST_outbound");

    let sip = dialer
        .dial(&DialRequest::new("sip:desk@pbx.example.com".parse().unwrap()))
        .await
        .unwrap_err();
    assert!(matches!(sip, RealtimeError::ConfigError(_)), "{sip}");
    let no_room = dialer
        .dial(&DialRequest::new(CallTarget::phone("+14155550199").unwrap()))
        .await
        .unwrap_err();
    assert!(no_room.to_string().contains("room"), "{no_room}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_busy_callee_is_an_error_carrying_the_sip_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CREATE_PARTICIPANT))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "code": "unavailable",
            "msg": "twirp error unknown: sip call failed",
            "meta": { "sip_status_code": "486", "sip_status": "Busy Here" }
        })))
        .mount(&server)
        .await;

    let error = livekit(&server)
        .dialer("ST_outbound")
        .with_room("r")
        .dial(&DialRequest::new(CallTarget::phone("+14155550199").unwrap()))
        .await
        .unwrap_err();

    match error {
        RealtimeError::ServerError { code, message } => {
            assert_eq!(code, "unavailable");
            assert!(message.contains("SIP 486"), "{message}");
        }
        other => panic!("expected a server error, got {other:?}"),
    }
}

#[tokio::test]
async fn transfer_refers_the_caller_and_hang_up_removes_them() {
    let server = MockServer::start().await;
    twirp(&server, TRANSFER, ()).await;
    twirp(&server, REMOVE, proto::RemoveParticipantResponse::default()).await;
    let call = livekit(&server).call(ROOM, CALLER);

    call.transfer(&CallTarget::phone("+14155550100").unwrap()).await.unwrap();
    call.hang_up().await.unwrap();

    let transfer = &requests_to(&server, TRANSFER).await[0];
    let request = proto::TransferSipParticipantRequest::decode(&transfer.body[..]).unwrap();
    assert_eq!(
        (
            request.room_name.as_str(),
            request.participant_identity.as_str(),
            request.transfer_to.as_str()
        ),
        (ROOM, CALLER, "tel:+14155550100")
    );
    assert!(!request.play_dialtone);
    let grants = claims(transfer);
    assert!(grants.sip.call && grants.video.room_admin && grants.video.room == ROOM, "{grants:?}");

    let remove = &requests_to(&server, REMOVE).await[0];
    let request = proto::RoomParticipantIdentity::decode(&remove.body[..]).unwrap();
    assert_eq!((request.room.as_str(), request.identity.as_str()), (ROOM, CALLER));
}

#[tokio::test]
async fn trunks_and_dispatch_rules_are_provisioned() {
    let server = MockServer::start().await;
    twirp(
        &server,
        INBOUND_TRUNK,
        proto::SipInboundTrunkInfo { sip_trunk_id: "ST_in".into(), ..Default::default() },
    )
    .await;
    twirp(
        &server,
        DISPATCH_RULE,
        proto::SipDispatchRuleInfo { sip_dispatch_rule_id: "SDR_1".into(), ..Default::default() },
    )
    .await;
    let livekit = livekit(&server);

    let trunk = livekit
        .create_inbound_trunk("front desk", vec!["+15105550100".into()], Default::default())
        .await
        .unwrap();
    let rule = livekit
        .create_dispatch_rule(
            "call-",
            livekit_api::services::sip::CreateSIPDispatchRuleOptions {
                name: "front desk".into(),
                trunk_ids: vec![trunk.sip_trunk_id.clone()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(
        (trunk.sip_trunk_id.as_str(), rule.sip_dispatch_rule_id.as_str()),
        ("ST_in", "SDR_1")
    );
    let request = proto::CreateSipInboundTrunkRequest::decode(
        &requests_to(&server, INBOUND_TRUNK).await[0].body[..],
    )
    .unwrap();
    let trunk = request.trunk.unwrap();
    assert_eq!(
        (trunk.name.as_str(), trunk.numbers.clone()),
        ("front desk", vec!["+15105550100".to_string()])
    );
    let request = proto::CreateSipDispatchRuleRequest::decode(
        &requests_to(&server, DISPATCH_RULE).await[0].body[..],
    )
    .unwrap();
    let rule = request.dispatch_rule.unwrap();
    assert_eq!(rule.trunk_ids, vec!["ST_in".to_string()]);
    assert_eq!(
        rule.rule.and_then(|rule| rule.rule),
        Some(proto::sip_dispatch_rule::Rule::DispatchRuleIndividual(
            proto::SipDispatchRuleIndividual { room_prefix: "call-".into(), ..Default::default() }
        ))
    );
    assert!(claims(&requests_to(&server, DISPATCH_RULE).await[0]).sip.admin);
}

// ===== Webhooks =====

/// A `participant_joined` delivery for a SIP participant, as LiveKit's webhook JSON names it.
fn participant_joined(rule_id: &str) -> String {
    json!({
        "event": "participant_joined",
        "id": "EV_1",
        "createdAt": "1750287018",
        "room": { "sid": "RM_1", "name": ROOM },
        "participant": {
            "sid": "PA_1",
            "identity": CALLER,
            "kind": "SIP",
            "attributes": {
                "sip.callID": "SCL_in_1",
                "sip.phoneNumber": "+14155550123",
                "sip.trunkPhoneNumber": "+15105550100",
                "sip.trunkID": "ST_in",
                "sip.ruleID": rule_id,
                "sip.callStatus": "active"
            }
        }
    })
    .to_string()
}

/// The `Authorization` token LiveKit signs a delivery of `body` with.
fn sign(body: &str) -> String {
    let digest = sha2::Sha256::digest(body.as_bytes());
    AccessToken::with_api_key(API_KEY, API_SECRET)
        .with_sha256(&base64::engine::general_purpose::STANDARD.encode(digest))
        .to_jwt()
        .unwrap()
}

#[test]
fn an_inbound_sip_participant_is_an_incoming_call() {
    let body = participant_joined("SDR_1");
    let event =
        LiveKitWebhookVerifier::new(API_KEY, API_SECRET).verify(&body, &sign(&body)).unwrap();

    assert_eq!(
        event,
        LiveKitWebhookEvent::IncomingCall(
            IncomingCall::new("livekit", "SCL_in_1")
                .with_attribute(ROOM_ATTRIBUTE, ROOM)
                .with_attribute(PARTICIPANT_IDENTITY_ATTRIBUTE, CALLER)
                .with_attribute("sip.callID", "SCL_in_1")
                .with_attribute("sip.phoneNumber", "+14155550123")
                .with_attribute("sip.trunkPhoneNumber", "+15105550100")
                .with_attribute("sip.trunkID", "ST_in")
                .with_attribute("sip.ruleID", "SDR_1")
        )
    );
}

#[test]
fn outbound_participants_and_other_events_are_not_incoming_calls() {
    let verifier = LiveKitWebhookVerifier::new(API_KEY, API_SECRET);
    let outbound = participant_joined("");
    assert_eq!(
        verifier.verify(&outbound, &format!("Bearer {}", sign(&outbound))).unwrap(),
        LiveKitWebhookEvent::Other { event: "participant_joined".into() }
    );
    let finished = json!({ "event": "room_finished", "room": { "name": ROOM } }).to_string();
    assert_eq!(
        verifier.verify(&finished, &sign(&finished)).unwrap(),
        LiveKitWebhookEvent::Other { event: "room_finished".into() }
    );
}

#[test]
fn a_tampered_or_foreign_webhook_is_rejected() {
    let body = participant_joined("SDR_1");
    let verifier = LiveKitWebhookVerifier::new(API_KEY, API_SECRET);

    let tampered = body.replace("+14155550123", "+19005550100");
    assert!(matches!(verifier.verify(&tampered, &sign(&body)), Err(RealtimeError::AuthError(_))));
    let foreign = AccessToken::with_api_key(API_KEY, "another-secret-long-enough-for-hs256")
        .with_sha256("x")
        .to_jwt()
        .unwrap();
    assert!(matches!(verifier.verify(&body, &foreign), Err(RealtimeError::AuthError(_))));
}

/// LiveKit has connected the caller when the webhook arrives: answering returns the handle,
/// and declining removes the caller.
#[tokio::test]
async fn answering_returns_the_callers_handle_and_declining_removes_them() {
    let server = MockServer::start().await;
    twirp(&server, REMOVE, proto::RemoveParticipantResponse::default()).await;
    let body = participant_joined("SDR_1");
    let LiveKitWebhookEvent::IncomingCall(call) =
        LiveKitWebhookVerifier::new(API_KEY, API_SECRET).verify(&body, &sign(&body)).unwrap()
    else {
        panic!("an incoming call");
    };
    let livekit = livekit(&server);

    let handle = livekit.answer(&call, &RealtimeConfig::default()).await.unwrap();
    assert_eq!(handle.call_id(), "SCL_in_1");
    livekit.decline(&call, 486).await.unwrap();

    let remove = &requests_to(&server, REMOVE).await[0];
    let request = proto::RoomParticipantIdentity::decode(&remove.body[..]).unwrap();
    assert_eq!((request.room.as_str(), request.identity.as_str()), (ROOM, CALLER));
}

// ===== A simulated call through the governed path =====

#[derive(Debug)]
struct Approve;

#[async_trait]
impl ToolConfirmationHandler for Approve {
    async fn decide(
        &self,
        _request: &ToolConfirmationRequest,
    ) -> adk_core::Result<ToolConfirmationDecision> {
        Ok(ToolConfirmationDecision::Approve)
    }
}

/// Runs a voice agent with `tools` whose model (a script standing in for the provider the
/// bridge put in the room) plays `script`, under `config` and `governance`; a freeze hangs up
/// `call` while the returned guard is held.
async fn run_agent(
    tools: Vec<Arc<dyn Tool>>,
    script: Vec<adk_realtime::ServerEvent>,
    config: RunConfig,
    governance: GovernanceControl,
    call: Arc<dyn CallControl>,
) -> (Vec<adk_core::Result<Event>>, adk_realtime::telephony::CallFreezeGuard) {
    let model = ScriptedRealtimeModel::new(vec![ScriptedReply::new(script).on_connect()])
        .with_idle_timeout(Duration::from_millis(100));
    let mut agent = RealtimeAgent::builder("receptionist").model(Arc::new(model));
    for tool in tools {
        agent = agent.tool(tool);
    }
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "front-desk".into(),
            user_id: "caller".into(),
            session_id: Some("call-1".into()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("front-desk")
        .agent(Arc::new(agent.build().unwrap()))
        .session_service(sessions as Arc<dyn SessionService>)
        .run_config(config)
        .governance(governance)
        .build()
        .unwrap();
    let guard = hang_up_on_freeze(call, [runner.governance().clone()]);
    let events =
        runner.run_str("caller", "call-1", Content::new("user")).await.unwrap().collect().await;
    (events, guard)
}

fn tool_responses(events: &[adk_core::Result<Event>]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| event.as_ref().ok())
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_approved_transfer_refers_the_livekit_caller_once() {
    let server = MockServer::start().await;
    twirp(&server, TRANSFER, ()).await;
    let call: Arc<dyn CallControl> = Arc::new(livekit(&server).call(ROOM, CALLER));
    let transfer: Arc<dyn Tool> = Arc::new(
        TransferCallTool::new(call.clone())
            .destination("billing", CallTarget::phone("+14155550100").unwrap()),
    );
    let policy = DeclarativePolicy::builder()
        .require_approval("transfer_call", "a person approves transfers")
        .build();

    let (events, _guard) = run_agent(
        vec![transfer],
        scripted::calls("fc_1", "transfer_call", json!({ "destination": "billing" })),
        RunConfig::builder()
            .tool_policy(Arc::new(policy))
            .tool_confirmation_handler(Arc::new(Approve))
            .build(),
        GovernanceControl::new(),
        call,
    )
    .await;

    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(
        tool_responses(&events),
        vec![json!({ "status": "transferred", "destination": "billing" })]
    );
    assert_eq!(requests_to(&server, TRANSFER).await.len(), 1);
}

#[tokio::test]
async fn a_freeze_removes_the_livekit_caller() {
    let server = MockServer::start().await;
    twirp(&server, TRANSFER, ()).await;
    twirp(&server, REMOVE, proto::RemoveParticipantResponse::default()).await;
    let call: Arc<dyn CallControl> = Arc::new(livekit(&server).call(ROOM, CALLER));
    let transfer: Arc<dyn Tool> = Arc::new(
        TransferCallTool::new(call.clone())
            .destination("billing", CallTarget::phone("+14155550100").unwrap()),
    );
    let control = GovernanceControl::new();
    control.freeze("incident 4101");

    let (events, _guard) = run_agent(
        vec![transfer],
        scripted::calls("fc_1", "transfer_call", json!({ "destination": "billing" })),
        RunConfig::default(),
        control,
        call,
    )
    .await;

    let error = events.iter().find_map(|event| event.as_ref().err()).expect("the run ends");
    assert_eq!(error.code, adk_core::GOVERNANCE_FROZEN_CODE);
    assert!(requests_to(&server, TRANSFER).await.is_empty());
    for _ in 0..100 {
        if !requests_to(&server, REMOVE).await.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the caller was not removed after the freeze");
}

/// An agent places an outbound call through LiveKit only to a configured contact, into the
/// configured room.
#[tokio::test]
async fn place_call_dials_through_the_livekit_trunk() {
    let server = MockServer::start().await;
    twirp(
        &server,
        CREATE_PARTICIPANT,
        proto::SipParticipantInfo { sip_call_id: "SCL_out".into(), ..Default::default() },
    )
    .await;
    let livekit = livekit(&server);
    let place: Arc<dyn Tool> = Arc::new(
        PlaceCallTool::new(Arc::new(livekit.dialer("ST_outbound")))
            .contact("on-call engineer", CallTarget::phone("+14155550199").unwrap())
            .attribute(ROOM_ATTRIBUTE, "incident-4101"),
    );

    let (events, _guard) = run_agent(
        vec![place],
        scripted::calls("fc_1", "place_call", json!({ "contact": "on-call engineer" })),
        RunConfig::default(),
        GovernanceControl::new(),
        Arc::new(livekit.call(ROOM, CALLER)),
    )
    .await;

    assert_eq!(
        tool_responses(&events),
        vec![json!({ "status": "placed", "contact": "on-call engineer", "call_id": "SCL_out" })]
    );
    let request = proto::CreateSipParticipantRequest::decode(
        &requests_to(&server, CREATE_PARTICIPANT).await[0].body[..],
    )
    .unwrap();
    assert_eq!(
        (request.sip_call_to.as_str(), request.room_name.as_str()),
        ("+14155550199", "incident-4101")
    );
}
