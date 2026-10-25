//! Scenario S12, money inside the box (spec 3.5 §5, §7.14, D-10, A-I16), scripted.
//!
//! A month of model tokens through the use-based rail and a hosting and domain renewal
//! within caps settle and reconcile; an advertising top-up above its category cap, asked
//! for by an agent on the governed path, is held for the owner and pays once the owner
//! approves; a burst of token spend at three times the daily average pauses the category
//! with a P1 alarm; and nothing the agent holds adds a payment method or raises a cap.

#![cfg(feature = "wallet")]

#[path = "support/wallet.rs"]
mod wallet_support;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::LlmAgentBuilder;
use adk_auth::{ScopeGuard, StaticScopeResolver};
use adk_core::identity::{SessionId, UserId};
use adk_core::{
    Agent, ApprovalScope, ApprovalStore, Content, Event, InMemoryApprovalStore, Llm, Part,
    RunConfig, ToolApproval,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_payments::auth::PAYMENT_EXECUTE_SCOPE;
use adk_payments::wallet::{
    HoldReason, InMemoryUseBasedScheme, PaymentChallenge, PaymentRequest, PaymentState,
    SpendCategory, StatementKind, UseBasedScheme, WALLET_CATEGORY_PAUSED_CODE, WALLET_PAY_TOOL,
    WALLET_STATUS_TOOL, WalletToolset,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use awp_types::Urgency;
use futures::StreamExt;
use serde_json::{Value, json};
use wallet_support::*;

const APP: &str = "site-1";
const SESSION: &str = "growth-session";

/// A metered model API that answers `402 Payment Required` until a request carries a
/// payment for its price.
struct MeteredModelApi {
    scheme: InMemoryUseBasedScheme,
    price: u64,
    served: AtomicUsize,
}

impl MeteredModelApi {
    fn requirement(&self) -> Value {
        json!({
            "x402Version": 1,
            "error": "X-PAYMENT header is required",
            "accepts": [{
                "scheme": "exact",
                "network": "local",
                "resource": MODELS_HOST,
                "payTo": MODELS_PAYEE,
                "maxAmountRequired": self.price.to_string(),
                "asset": "USD",
                "description": "one completion",
                "maxTimeoutSeconds": 60
            }]
        })
    }

    /// Answers a request, with the `X-PAYMENT` header value when it carries one.
    fn call(&self, payment: Option<&str>) -> (u16, Value) {
        let requirement = self.requirement();
        let Some(payment) = payment else {
            return (402, requirement);
        };
        let challenge = self.scheme.challenges(&requirement).unwrap().remove(0);
        if self.scheme.verify(&challenge, payment) {
            self.served.fetch_add(1, Ordering::SeqCst);
            (200, json!({ "completion": "ok" }))
        } else {
            (402, requirement)
        }
    }
}

/// One metered call: the 402, the wallet's payment, the retry with the proof.
async fn metered_call(f: &Fixture, api: &MeteredModelApi, key: String) -> adk_core::Result<u16> {
    let (status, body) = api.call(None);
    assert_eq!(status, 402);
    let challenge: PaymentChallenge = f.scheme.challenges(&body)?.remove(0);
    let outcome =
        f.wallet.pay_challenge(key, SpendCategory::Models, &challenge, &finance()).await?;
    let proof = outcome.record().use_based_proof().expect("a settled payment carries its proof");
    Ok(api.call(Some(&proof.value)).0)
}

async fn run(runner: &Runner, text: &str) -> Vec<Event> {
    let mut stream = runner
        .run(
            UserId::try_from(OWNER).unwrap(),
            SessionId::try_from(SESSION).unwrap(),
            Content::new("user").with_text(text),
        )
        .await
        .expect("the run starts");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("agent event"));
    }
    events
}

fn answer_to(events: &[Event], call_id: &str) -> Option<Value> {
    events
        .iter()
        .flat_map(|event| event.llm_response.content.iter().flat_map(|content| &content.parts))
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, id, .. }
                if id.as_deref() == Some(call_id) =>
            {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .next_back()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s12_money_stays_inside_the_box() {
    let f = Fixture::new(s12_policy(), at(2026, 9, 1, 8));
    let policy_before = f.wallet.policy().clone();
    let api = MeteredModelApi { scheme: f.scheme.clone(), price: 250, served: AtomicUsize::new(0) };

    // September: four metered completions a day, hosting on the 1st, the domain on the 4th.
    for day in 0..30 {
        for call in 0..4 {
            let status = metered_call(&f, &api, format!("sep-{day}-{call}")).await.unwrap();
            assert_eq!(status, 200, "a paid completion is served");
        }
        if day == 0 {
            let renewal = PaymentRequest::new(
                "host-2026-09",
                SpendCategory::Hosting,
                adk_payments::wallet::Payee::new("hostco"),
                2_000,
                "USD",
            )
            .with_description("September hosting");
            assert!(f.wallet.pay(renewal, &finance()).await.unwrap().is_settled());
        }
        if day == 3 {
            let renewal = PaymentRequest::new(
                "example-com-2027",
                SpendCategory::Domains,
                adk_payments::wallet::Payee::new("registrar"),
                1_500,
                "USD",
            )
            .with_description("example.com renewal");
            assert!(f.wallet.pay(renewal, &finance()).await.unwrap().is_settled());
        }
        f.advance_hours(24);
    }
    assert_eq!(api.served.load(Ordering::SeqCst), 120);

    // 1 October: September reconciles against the treasury and both billing providers.
    let report = f.wallet.reconcile_due().await.unwrap();
    assert!(report.is_clean(), "{:?}", report.discrepancies);
    assert_eq!((report.payments, report.matched), (122, 122));
    assert_eq!(report.wallet_total, 120 * 250 + 2_000 + 1_500);
    assert_eq!(report.statement_total, i128::from(report.wallet_total));

    // The Growth agent asks for a 300 USD advertising top-up above the 200 USD cap.
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        [
            ScriptedResponse::function_call(
                WALLET_PAY_TOOL,
                json!({
                    "category": "advertising",
                    "payee": { "id": "adsco" },
                    "amount": 30_000,
                    "currency": "USD",
                    "description": "October campaign top-up"
                }),
            ),
            ScriptedResponse::function_calls([
                ("wallet_raise_cap", json!({ "category": "advertising", "cap": 1_000_000 })),
                (
                    WALLET_PAY_TOOL,
                    json!({
                        "category": "advertising",
                        "payee": { "id": "adsco" },
                        "amount": 100,
                        "currency": "USD",
                        "description": "add a card",
                        "addPaymentMethod": { "card": "4242424242424242" }
                    }),
                ),
            ]),
            ScriptedResponse::text("The top-up is paid; caps and payment methods are the owner's."),
        ],
    ));
    let guard = ScopeGuard::new(StaticScopeResolver::new(vec![PAYMENT_EXECUTE_SCOPE.to_string()]));
    let tools = WalletToolset::new(f.wallet.clone()).with_scope_guard(guard).tools();
    let names: Vec<_> = tools.iter().map(|tool| tool.name().to_string()).collect();
    assert_eq!(
        names,
        vec![WALLET_PAY_TOOL, WALLET_STATUS_TOOL],
        "the agent's whole wallet surface"
    );
    let mut builder = LlmAgentBuilder::new("growth").model(model.clone() as Arc<dyn Llm>);
    for tool in tools {
        builder = builder.tool(tool);
    }
    let agent = builder.build().unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: OWNER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(sessions)
        .run_config(RunConfig::builder().approval_store(approvals.clone()).build())
        .build()
        .unwrap();
    let scope = ApprovalScope::new(APP, OWNER, SESSION);

    let events = run(&runner, "Top up the October ad campaign by 300 USD.").await;
    let held: Vec<_> =
        events.iter().filter_map(|event| event.actions.tool_confirmation.clone()).collect();
    assert_eq!(held.len(), 1, "the top-up is held as the call");
    assert_eq!(held[0].tool_name, WALLET_PAY_TOOL);
    let pending = approvals.pending(&scope).await.unwrap();
    assert_eq!(pending.len(), 1, "the owner's inbox has the card");
    let waiting = f.wallet.held().await.unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(
        waiting[0].hold,
        Some(HoldReason::OverCategoryCap { cap: 20_000, spent: 0, requested: 30_000 })
    );
    assert!(f.adsco.lines().is_empty(), "nothing was charged while held");

    // The owner approves on their phone; the next run resumes the held call and pays it.
    approvals.decide(&scope, &pending[0].fingerprint, ToolApproval::approve()).await.unwrap();
    let events = run(&runner, "Approved.").await;
    let answer = answer_to(&events, "scripted_tc_0_0").expect("the held call is answered");
    assert_eq!(answer["status"], "settled", "{answer}");
    assert_eq!(answer["ownerApproved"], true);
    let paid = f.wallet.payment(answer["paymentId"].as_str().unwrap()).await.unwrap().unwrap();
    assert_eq!(paid.state, PaymentState::Settled);
    let decision = paid.decision.expect("the owner's decision is recorded");
    assert_eq!(decision.source, "governed_path");
    assert_eq!(decision.decided_by, adk_core::SubjectRef::human(OWNER));
    assert_eq!(f.adsco.lines().iter().map(|line| line.amount).sum::<u64>(), 30_000, "paid once");

    // The agent then tried to raise the cap and to add a card: there is no tool for the
    // first, and the pay tool refuses arguments beyond its schema.
    let raise = answer_to(&events, "scripted_tc_1_0").expect("the raise attempt is answered");
    assert!(raise.to_string().contains("not found"), "{raise}");
    let card = answer_to(&events, "scripted_tc_1_1").expect("the card attempt is answered");
    assert!(card.to_string().contains("addPaymentMethod"), "{card}");
    assert_eq!(f.adsco.lines().len(), 1, "the refused call paid nothing");

    // 2 October: a burst of token spend at three times the daily average.
    f.advance_hours(24);
    let mut settled = 0;
    let error = loop {
        match metered_call(&f, &api, format!("oct-2-{settled}")).await {
            Ok(status) => {
                assert_eq!(status, 200);
                settled += 1;
            }
            Err(error) => break error,
        }
        assert!(settled < 20, "the burst is stopped");
    };
    assert_eq!(error.code, WALLET_CATEGORY_PAUSED_CODE);
    assert_eq!(
        settled, 11,
        "11 x 2.50 USD settle; the 12th would pass 3x the trailing daily average"
    );
    let alarms = f.alarms.alarms();
    assert_eq!(alarms.len(), 1);
    assert_eq!(alarms[0].urgency(), Urgency::P1);
    let status = f.wallet.status().await.unwrap();
    let models = status.iter().find(|s| s.category == SpendCategory::Models).unwrap();
    assert!(models.paused.is_some(), "the category is paused");
    assert!(metered_call(&f, &api, "oct-2-after".into()).await.is_err(), "and stays paused");

    // Nothing changed a cap, a payee or a payment method.
    assert_eq!(f.wallet.policy(), &policy_before);
    let advertising = status.iter().find(|s| s.category == SpendCategory::Advertising).unwrap();
    assert_eq!(advertising.cap_per_period, 20_000);
    assert!(f.provider.entries().iter().all(|entry| entry.kind != StatementKind::Funding));
    let actions = f.audit_actions();
    for action in ["wallet.payment", "wallet.held", "wallet.anomaly", "wallet.reconciled"] {
        assert!(actions.iter().any(|recorded| recorded == action), "{action} is audited");
    }
}
