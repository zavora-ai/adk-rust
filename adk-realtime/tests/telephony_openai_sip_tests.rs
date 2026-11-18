//! OpenAI Realtime SIP, against the documented API: signed `realtime.call.incoming` webhooks,
//! the call-control requests (wiremock), the WebSocket that attaches to a call (a local
//! socket), and a simulated call end to end, in which a scripted model stands in for the
//! session and asks to transfer the caller through the governed path.
//!
//! Payloads follow <https://developers.openai.com/api/docs/guides/realtime-sip>.

#![cfg(all(feature = "openai-sip", feature = "scripted"))]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::webhook::{InMemoryWebhookReplayGuard, WebhookSecret, WebhookSigner};
use adk_core::{
    ActionLedger, ActionOutcome, Content, DeclarativePolicy, Event, GovernanceControl,
    InMemoryActionLedger, Part, RunConfig, Tool, ToolConfirmationDecision, ToolConfirmationHandler,
    ToolConfirmationRequest,
};
use adk_realtime::openai::OpenAIRealtimeModel;
use adk_realtime::openai::sip::{OpenAISipClient, OpenAIWebhookEvent, OpenAIWebhookVerifier};
use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply};
use adk_realtime::telephony::{
    CallControl, CallFreezeGuard, CallTarget, DialRequest, EndCallTool, IncomingCall,
    OutboundDialer, PlaceCallTool, SipHeader, TransferCallTool, hang_up_on_freeze,
};
use adk_realtime::{RealtimeAgent, RealtimeConfig, RealtimeError, RealtimeModel};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const SECRET: &str = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
const API_KEY: &str = "sk-test";
const CALL_ID: &str = "some_unique_id";

/// The `realtime.call.incoming` payload from OpenAI's SIP guide.
const INCOMING_CALL: &str = r#"{
  "object": "event",
  "id": "evt_685343a1381c819085d44c354e1b330e",
  "type": "realtime.call.incoming",
  "created_at": 1750287018,
  "data": {
    "call_id": "some_unique_id",
    "sip_headers": [
      { "name": "From", "value": "sip:+142555512112@sip.example.com" },
      { "name": "To", "value": "sip:+18005551212@sip.example.com" },
      { "name": "Call-ID", "value": "03782086-4ce9-44bf-8b0d-4e303d2cc590" }
    ]
  }
}"#;

/// Signs `body` as OpenAI would deliver it under `id` now, keyed with `secret`.
fn deliver(secret: WebhookSecret, id: &str, body: &str) -> [(&'static str, String); 3] {
    WebhookSigner::new(secret).sign_now(id, body.as_bytes()).header_pairs()
}

async fn verify(
    verifier: &OpenAIWebhookVerifier,
    headers: &[(&'static str, String)],
    body: &str,
) -> adk_realtime::Result<OpenAIWebhookEvent> {
    let header = |name: &'static str| {
        headers.iter().find(|(key, _)| *key == name).map(|(_, value)| value.as_str())
    };
    verifier.verify(header, body.as_bytes()).await
}

fn sip_client(server: &MockServer) -> OpenAISipClient {
    OpenAISipClient::new(API_KEY).with_base_url(format!("{}/v1", server.uri()))
}

fn call_path(action: &str) -> String {
    format!("/v1/realtime/calls/{CALL_ID}/{action}")
}

// ===== Webhooks =====

#[tokio::test]
async fn the_documented_incoming_call_webhook_verifies() {
    let verifier = OpenAIWebhookVerifier::new(SECRET).unwrap();
    let headers = deliver(WebhookSecret::from_base64(SECRET).unwrap(), "wh_1", INCOMING_CALL);

    let event = verify(&verifier, &headers, INCOMING_CALL).await.unwrap();

    let header = |name: &str, value: &str| SipHeader { name: name.into(), value: value.into() };
    assert_eq!(
        event,
        OpenAIWebhookEvent::IncomingCall(IncomingCall::new("openai", CALL_ID).with_sip_headers(
            vec![
                header("From", "sip:+142555512112@sip.example.com"),
                header("To", "sip:+18005551212@sip.example.com"),
                header("Call-ID", "03782086-4ce9-44bf-8b0d-4e303d2cc590"),
            ]
        ))
    );
}

/// OpenAI's SDKs key a secret without the `whsec_` prefix with its UTF-8 bytes.
#[tokio::test]
async fn a_secret_without_the_whsec_prefix_is_used_as_its_bytes() {
    let raw = "plain-signing-secret";
    let verifier = OpenAIWebhookVerifier::new(raw).unwrap();
    let headers =
        deliver(WebhookSecret::from_bytes(raw.as_bytes()).unwrap(), "wh_1", INCOMING_CALL);

    assert!(verify(&verifier, &headers, INCOMING_CALL).await.is_ok());
}

#[tokio::test]
async fn a_forged_or_tampered_delivery_is_rejected() {
    let verifier = OpenAIWebhookVerifier::new(SECRET).unwrap();

    let forged = deliver(
        WebhookSecret::from_bytes(b"not the secret".to_vec()).unwrap(),
        "wh_1",
        INCOMING_CALL,
    );
    let error = verify(&verifier, &forged, INCOMING_CALL).await.unwrap_err();
    assert!(matches!(error, RealtimeError::AuthError(_)), "{error}");

    let signed = deliver(WebhookSecret::from_base64(SECRET).unwrap(), "wh_1", INCOMING_CALL);
    let tampered = INCOMING_CALL.replace("some_unique_id", "another_call");
    let error = verify(&verifier, &signed, &tampered).await.unwrap_err();
    assert!(matches!(error, RealtimeError::AuthError(_)), "{error}");

    let missing = [signed[0].clone(), signed[1].clone()];
    let error = verify(&verifier, &missing, INCOMING_CALL).await.unwrap_err();
    assert!(error.to_string().contains("webhook-signature"), "{error}");
}

#[tokio::test]
async fn a_stale_delivery_is_rejected() {
    let verifier = OpenAIWebhookVerifier::new(SECRET).unwrap();
    let stale = chrono::Utc::now().timestamp() - 3_600;
    let signed = WebhookSigner::new(WebhookSecret::from_base64(SECRET).unwrap())
        .sign("wh_1", stale, INCOMING_CALL.as_bytes())
        .header_pairs();

    let error = verify(&verifier, &signed, INCOMING_CALL).await.unwrap_err();
    assert!(error.to_string().contains("3600s old"), "{error}");
}

/// OpenAI retries under the same `webhook-id` and may deliver twice; a call is answered once.
#[tokio::test]
async fn a_redelivered_webhook_is_refused_by_the_replay_guard() {
    let verifier = OpenAIWebhookVerifier::new(SECRET)
        .unwrap()
        .with_replay_guard(Arc::new(InMemoryWebhookReplayGuard::new()));
    let headers = deliver(WebhookSecret::from_base64(SECRET).unwrap(), "wh_1", INCOMING_CALL);

    assert!(verify(&verifier, &headers, INCOMING_CALL).await.is_ok());
    let error = verify(&verifier, &headers, INCOMING_CALL).await.unwrap_err();
    assert!(error.to_string().contains("already delivered"), "{error}");
}

#[tokio::test]
async fn other_event_types_are_passed_through() {
    let body = r#"{"object":"event","id":"evt_2","type":"response.completed","created_at":1,"data":{"id":"resp_1"}}"#;
    let verifier = OpenAIWebhookVerifier::new(SECRET).unwrap();
    let headers = deliver(WebhookSecret::from_base64(SECRET).unwrap(), "wh_2", body);

    assert_eq!(
        verify(&verifier, &headers, body).await.unwrap(),
        OpenAIWebhookEvent::Other {
            event_type: "response.completed".into(),
            data: json!({ "id": "resp_1" }),
        }
    );
}

// ===== Call control =====

#[tokio::test]
async fn accept_sends_the_session_with_the_model_and_no_audio_format() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(call_path("accept")))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let config = RealtimeConfig::default()
        .with_instruction("You are Alex, a friendly concierge for Example Corp.")
        .with_voice("marin");

    let call = sip_client(&server).accept(CALL_ID, &config).await.unwrap();

    assert_eq!(call.call_id(), CALL_ID);
    let requests = server.received_requests().await.unwrap();
    let body: Value = requests[0].body_json().unwrap();
    assert_eq!(body["type"], "realtime");
    assert_eq!(body["model"], "gpt-realtime-2.1");
    assert_eq!(body["instructions"], "You are Alex, a friendly concierge for Example Corp.");
    assert_eq!(body["audio"]["output"], json!({ "voice": "marin" }));
    assert!(body["audio"]["input"].get("format").is_none(), "{body}");
}

#[tokio::test]
async fn reject_refer_and_hangup_send_the_documented_bodies() {
    let server = MockServer::start().await;
    for (action, expected) in [
        ("reject", Some(json!({ "status_code": 486 }))),
        ("refer", Some(json!({ "target_uri": "tel:+14155550123" }))),
    ] {
        Mock::given(method("POST"))
            .and(path(call_path(action)))
            .and(body_json(expected.unwrap()))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path(call_path("hangup")))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let client = sip_client(&server);

    client.reject(CALL_ID, Some(486)).await.unwrap();
    client.refer(CALL_ID, &CallTarget::phone("+14155550123").unwrap()).await.unwrap();
    client.call(CALL_ID).hang_up().await.unwrap();

    let hangup = server.received_requests().await.unwrap().pop().unwrap();
    assert!(hangup.body.is_empty(), "hangup has no body");
    let error = client.reject(CALL_ID, Some(200)).await.unwrap_err();
    assert!(matches!(error, RealtimeError::ConfigError(_)), "{error}");
}

#[tokio::test]
async fn openai_errors_keep_their_code_and_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(call_path("refer")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": { "message": "Call not found", "code": "call_not_found" }
        })))
        .mount(&server)
        .await;

    let error = sip_client(&server)
        .refer(CALL_ID, &CallTarget::phone("+14155550123").unwrap())
        .await
        .unwrap_err();

    match error {
        RealtimeError::ServerError { code, message } => {
            assert_eq!((code.as_str(), message.as_str()), ("call_not_found", "Call not found"));
        }
        other => panic!("expected a server error, got {other:?}"),
    }
}

// ===== Attaching the session =====

/// The session joins the call at `?call_id=` with no `model`, and its `session.update` leaves
/// the audio format to the call, as OpenAI's guide and Agents SDK do.
#[tokio::test]
// tungstenite's handshake callback fixes its error type, `Response<Option<String>>`.
#[allow(clippy::result_large_err)]
async fn the_session_attaches_to_the_call_without_a_model_or_audio_format() {
    use tokio_tungstenite::tungstenite::handshake::server::{Request as WsRequest, Response};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut handshake = None;
        let mut ws = tokio_tungstenite::accept_hdr_async(
            socket,
            |request: &WsRequest, response: Response| {
                handshake = Some((
                    request.uri().to_string(),
                    request.headers().get("authorization").map(|v| v.to_str().unwrap().to_string()),
                ));
                Ok(response)
            },
        )
        .await
        .unwrap();
        let first = ws.next().await.unwrap().unwrap().into_text().unwrap().to_string();
        let _ = seen_tx.send((handshake.unwrap(), first));
        let _ = ws.close(None).await;
    });

    let client =
        OpenAISipClient::new(API_KEY).with_realtime_url(format!("ws://{address}/v1/realtime"));
    let config =
        RealtimeConfig::default().with_instruction("Greet the caller.").with_voice("marin");
    let session = client.realtime_model(CALL_ID).connect(config).await.unwrap();

    let ((uri, authorization), first) = seen_rx.await.unwrap();
    assert_eq!(uri, format!("/v1/realtime?call_id={CALL_ID}"));
    assert_eq!(authorization.as_deref(), Some("Bearer sk-test"));
    let update: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(update["type"], "session.update");
    assert_eq!(update["session"]["instructions"], "Greet the caller.");
    assert!(update["session"].get("model").is_none(), "{update}");
    assert!(update["session"]["audio"]["input"].get("format").is_none(), "{update}");
    assert!(update["session"]["audio"]["output"].get("format").is_none(), "{update}");
    session.close().await.unwrap();
}

#[test]
fn a_call_attached_model_has_no_model_parameter() {
    let model = OpenAIRealtimeModel::new(API_KEY, "gpt-realtime-2.1").with_call_id("rtc/1 a");
    assert_eq!(model.websocket_url(), "wss://api.openai.com/v1/realtime?call_id=rtc%2F1+a");
    assert_eq!(model.model_id(), "gpt-realtime-2.1", "usage is still priced by the model");
}

// ===== A simulated call, end to end =====

const APP: &str = "front-desk";
const USER: &str = "caller";
const SESSION: &str = "call-1";

/// Approves every held call, counting the decisions.
#[derive(Debug, Default)]
struct Approver(AtomicUsize);

#[async_trait]
impl ToolConfirmationHandler for Approver {
    async fn decide(
        &self,
        _request: &ToolConfirmationRequest,
    ) -> adk_core::Result<ToolConfirmationDecision> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolConfirmationDecision::Approve)
    }
}

/// Answers a call announced by a signed webhook, then runs a receptionist whose model (a
/// script standing in for the attached session) asks to transfer the caller to billing,
/// under `config`. Returns the run's events.
async fn simulated_call(
    server: &MockServer,
    script: Vec<adk_realtime::ServerEvent>,
    config: RunConfig,
    governance: Option<GovernanceControl>,
) -> (Vec<adk_core::Result<Event>>, Option<CallFreezeGuard>) {
    let verifier = OpenAIWebhookVerifier::new(SECRET).unwrap();
    let headers = deliver(WebhookSecret::from_base64(SECRET).unwrap(), "wh_call", INCOMING_CALL);
    let OpenAIWebhookEvent::IncomingCall(call) =
        verify(&verifier, &headers, INCOMING_CALL).await.unwrap()
    else {
        panic!("an incoming call");
    };
    let client = sip_client(server);
    let handle: Arc<dyn CallControl> = Arc::new(
        client
            .accept(
                &call.call_id,
                &RealtimeConfig::default().with_instruction("Answer for Example Corp."),
            )
            .await
            .unwrap(),
    );

    let transfer: Arc<dyn Tool> = Arc::new(
        TransferCallTool::new(handle.clone())
            .destination("billing", CallTarget::phone("+14155550123").unwrap()),
    );
    let tools: Vec<Arc<dyn Tool>> = vec![transfer, Arc::new(EndCallTool::new(handle.clone()))];
    run_agent(tools, script, config, governance, Some(handle)).await
}

/// Runs a voice agent with `tools` whose model (a script standing in for the session) plays
/// `script` once connected, under `config`; a freeze of the runner's governance hangs up
/// `call` while the returned guard is held. Returns the run's events.
async fn run_agent(
    tools: Vec<Arc<dyn Tool>>,
    script: Vec<adk_realtime::ServerEvent>,
    config: RunConfig,
    governance: Option<GovernanceControl>,
    call: Option<Arc<dyn CallControl>>,
) -> (Vec<adk_core::Result<Event>>, Option<CallFreezeGuard>) {
    let model = ScriptedRealtimeModel::new(vec![ScriptedReply::new(script).on_connect()])
        .with_idle_timeout(Duration::from_millis(100));
    let mut agent = RealtimeAgent::builder("receptionist").model(Arc::new(model));
    for tool in tools {
        agent = agent.tool(tool);
    }
    let agent = agent.build().unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(SESSION.into()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let mut builder = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .run_config(config);
    if let Some(control) = governance {
        builder = builder.governance(control);
    }
    let runner = builder.build().unwrap();
    let guard = call.map(|call| hang_up_on_freeze(call, [runner.governance().clone()]));
    let events = runner.run_str(USER, SESSION, Content::new("user")).await.unwrap().collect().await;
    (events, guard)
}

fn transfer_to_billing() -> Vec<adk_realtime::ServerEvent> {
    scripted::calls("fc_1", "transfer_call", json!({ "destination": "billing" }))
}

async fn mount_call_control(server: &MockServer) {
    for action in ["accept", "refer", "hangup"] {
        Mock::given(method("POST"))
            .and(path(call_path(action)))
            .respond_with(ResponseTemplate::new(200))
            .mount(server)
            .await;
    }
}

async fn requests_to(server: &MockServer, action: &str) -> Vec<Request> {
    let wanted = call_path(action);
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == wanted)
        .collect()
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
async fn a_transfer_the_policy_denies_never_reaches_openai() {
    let server = MockServer::start().await;
    mount_call_control(&server).await;
    let policy =
        DeclarativePolicy::builder().deny("transfer_call", "transfers are closed tonight").build();

    let (events, _guard) = simulated_call(
        &server,
        transfer_to_billing(),
        RunConfig::builder().tool_policy(Arc::new(policy)).build(),
        None,
    )
    .await;

    assert!(requests_to(&server, "refer").await.is_empty());
    let responses = tool_responses(&events);
    assert!(
        responses[0]["error"].as_str().unwrap().contains("transfers are closed tonight"),
        "{responses:?}"
    );
}

#[tokio::test]
async fn a_transfer_needing_approval_is_held_without_a_decision() {
    let server = MockServer::start().await;
    mount_call_control(&server).await;
    let policy = DeclarativePolicy::builder()
        .require_approval("transfer_call", "a person approves transfers")
        .build();

    let (events, _guard) = simulated_call(
        &server,
        transfer_to_billing(),
        RunConfig::builder().tool_policy(Arc::new(policy)).build(),
        None,
    )
    .await;

    assert!(requests_to(&server, "refer").await.is_empty());
    let held = events
        .iter()
        .filter_map(|event| event.as_ref().ok())
        .find_map(|event| event.actions.tool_confirmation.clone())
        .expect("the transfer is held for a person");
    assert_eq!(held.tool_name, "transfer_call");
    assert_eq!(held.args, json!({ "destination": "billing" }));
}

/// The whole path: a signed webhook, accept, a held transfer a person approves, one REFER to
/// the configured number, and one ledger record — a redelivered call is answered from it.
#[tokio::test]
async fn an_approved_transfer_refers_the_call_once_and_is_ledgered() {
    let server = MockServer::start().await;
    mount_call_control(&server).await;
    let approver = Arc::new(Approver::default());
    let ledger = Arc::new(InMemoryActionLedger::new());
    let policy = DeclarativePolicy::builder()
        .require_approval("transfer_call", "a person approves transfers")
        .build();
    let mut script = transfer_to_billing();
    script.extend(transfer_to_billing());

    let (events, _guard) = simulated_call(
        &server,
        script,
        RunConfig::builder()
            .tool_policy(Arc::new(policy))
            .tool_confirmation_handler(approver.clone())
            .action_ledger(ledger.clone() as Arc<dyn ActionLedger>)
            .build(),
        None,
    )
    .await;

    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(requests_to(&server, "accept").await.len(), 1);
    let refers = requests_to(&server, "refer").await;
    assert_eq!(refers.len(), 1, "the redelivered call is not referred again");
    assert_eq!(
        refers[0].body_json::<Value>().unwrap(),
        json!({ "target_uri": "tel:+14155550123" })
    );
    let responses = tool_responses(&events);
    assert_eq!(responses[0], json!({ "status": "transferred", "destination": "billing" }));
    assert_eq!(responses[1]["status"], "already_succeeded");
    assert!(approver.0.load(Ordering::SeqCst) >= 1);

    let invocation = events[0].as_ref().unwrap().invocation_id.clone();
    let record = ledger
        .get(&format!("{APP}/{USER}/{SESSION}/{invocation}/fc_1"))
        .await
        .unwrap()
        .expect("the transfer is ledgered");
    assert!(matches!(record.outcome, Some(ActionOutcome::Succeeded { .. })), "{record:?}");
}

/// A freeze ends the phone call itself, not only the agent's session: OpenAI would otherwise
/// keep talking to the caller with no agent answering its tool calls.
#[tokio::test]
async fn a_freeze_hangs_up_the_call_and_refuses_the_transfer() {
    let server = MockServer::start().await;
    mount_call_control(&server).await;
    let control = GovernanceControl::new();
    control.freeze("incident 4101: unexpected transfers");

    let (events, _guard) =
        simulated_call(&server, transfer_to_billing(), RunConfig::default(), Some(control)).await;

    let error = events.iter().find_map(|event| event.as_ref().err()).expect("the run ends");
    assert_eq!(error.code, adk_core::GOVERNANCE_FROZEN_CODE, "{error:?}");
    assert!(requests_to(&server, "refer").await.is_empty());
    for _ in 0..100 {
        if !requests_to(&server, "hangup").await.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the call was not hung up after the freeze");
}

// ===== Outbound calls =====

/// Records dial requests and answers each with a call handle.
#[derive(Debug, Default)]
struct RecordingDialer(std::sync::Mutex<Vec<DialRequest>>);

#[derive(Debug)]
struct Dialed(String);

#[async_trait]
impl CallControl for Dialed {
    fn call_id(&self) -> &str {
        &self.0
    }
    fn provider(&self) -> &str {
        "test"
    }
    fn capabilities(&self) -> adk_realtime::telephony::CallCapabilities {
        adk_realtime::telephony::CallCapabilities::default()
    }
    async fn transfer(&self, _target: &CallTarget) -> adk_realtime::Result<()> {
        Ok(())
    }
    async fn hang_up(&self) -> adk_realtime::Result<()> {
        Ok(())
    }
}

#[async_trait]
impl OutboundDialer for RecordingDialer {
    fn provider(&self) -> &str {
        "test"
    }
    async fn dial(&self, request: &DialRequest) -> adk_realtime::Result<Arc<dyn CallControl>> {
        let mut dialed = self.0.lock().unwrap();
        dialed.push(request.clone());
        Ok(Arc::new(Dialed(format!("out-{}", dialed.len()))))
    }
}

/// Dialling is a governed, non-idempotent call: it reaches only configured contacts and
/// stops at the tool's call limit.
#[tokio::test]
async fn place_call_dials_only_configured_contacts_up_to_its_limit() {
    let dialer = Arc::new(RecordingDialer::default());
    let tool = Arc::new(
        PlaceCallTool::new(dialer.clone())
            .contact("on-call engineer", CallTarget::phone("+14155550199").unwrap())
            .attribute("room", "incident-4101")
            .max_calls(1),
    );
    let mut script = scripted::calls("fc_1", "place_call", json!({ "contact": "+19005550100" }));
    script.extend(scripted::calls("fc_2", "place_call", json!({ "contact": "on-call engineer" })));
    script.extend(scripted::calls("fc_3", "place_call", json!({ "contact": "on-call engineer" })));

    let (events, _) =
        run_agent(vec![tool.clone() as Arc<dyn Tool>], script, RunConfig::default(), None, None)
            .await;

    let responses = tool_responses(&events);
    assert_eq!(responses.len(), 3, "{responses:?}");
    assert!(
        responses[0]["error"].as_str().unwrap().contains("telephony.unknown_destination"),
        "{responses:?}"
    );
    assert_eq!(
        responses[1],
        json!({ "status": "placed", "contact": "on-call engineer", "call_id": "out-1" })
    );
    assert!(
        responses[2]["error"].as_str().unwrap().contains("telephony.call_limit"),
        "{responses:?}"
    );
    let dialed = dialer.0.lock().unwrap().clone();
    assert_eq!(
        dialed,
        vec![
            DialRequest::new(CallTarget::phone("+14155550199").unwrap())
                .with_attribute("room", "incident-4101")
        ]
    );
    assert_eq!(tool.placed_calls().len(), 1);
}
