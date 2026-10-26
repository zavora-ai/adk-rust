//! Card hints and granted uses through the engine (spec 3.3 §5.7, §7.1, §7.2): a feature's
//! hint heads, words and details its own cards on every surface, may only make a card stricter
//! — take **Always allow** away, mark money — and never adds an answer or changes the risk;
//! an agent the home runs lists what the home grants it.

use std::sync::Arc;

use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope,
    ApprovalService, ApproverSelector, Event, EventActions, InMemoryApprovalStore, ManualClock,
    StoreApprovalService, ToolConfirmationRequest, ToolOrigin,
};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::rules::RuleViolation;
use adk_surface::{
    AgentDetail, AgentDraft, AgentLife, AgentRef, AlwaysAllowOffer, Answer, Avatar, Caller,
    CardDetail, CardHint, CardHints, Command, CommandOutcome, DecisionCard, FeedFrame, GrantedUse,
    LocalSource, Mode, ModeView, Origin, Place, Reversibility, Risk, Source, SurfaceEngine,
    SurfaceError, SurfaceKind, TimelineStep,
};
use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use futures::StreamExt;
use serde_json::{Value, json};

const APP: &str = "adk-rust";
const VERBS: &str = "verbs:adk-rust";

fn at(hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 26, hour, minute, 0).unwrap()
}

fn detail(id: &str, name: &str, state: AgentLife) -> AgentDetail {
    AgentDetail {
        id: id.into(),
        agent: AgentRef {
            name: id.into(),
            display_name: name.into(),
            avatar: Avatar { monogram: name[..1].into(), color: "#15803D".into() },
        },
        draft: AgentDraft::new(name, "work"),
        version: 1,
        state,
        origin_text: "Added by you on 25 Oct".into(),
        mode: ModeView {
            mode: Mode::AskFirst,
            label: "Ask first".into(),
            status: "Asks first".into(),
        },
        cant_start: None,
        changed_at: at(9, 0),
        changed_on: None,
        granted: Vec::new(),
    }
}

/// The in-process inbox and sessions, offering "Always allow" as a harness with standing rules
/// does, and keeping two agents the person added: Finance, which runs, and Old, removed.
struct Home {
    inner: LocalSource,
}

#[async_trait]
impl Source for Home {
    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    async fn transcript(
        &self,
        user: &str,
        session: &str,
    ) -> adk_surface::Result<Option<Vec<Event>>> {
        self.inner.transcript(user, session).await
    }

    async fn requests(
        &self,
        caller: &Caller,
        session: Option<&str>,
        open_only: bool,
    ) -> adk_surface::Result<Vec<ApprovalRequest>> {
        self.inner.requests(caller, session, open_only).await
    }

    async fn request(&self, id: &str) -> adk_surface::Result<Option<ApprovalRequest>> {
        self.inner.request(id).await
    }

    async fn decide(
        &self,
        id: &str,
        expected_version: u64,
        decision: ApprovalDecision,
    ) -> adk_surface::Result<ApprovalRequest> {
        self.inner.decide(id, expected_version, decision).await
    }

    async fn always_allow_offer(&self, _: &ApprovalRequest) -> Option<AlwaysAllowOffer> {
        Some(AlwaysAllowOffer { label: "Always allow for this person".into() })
    }

    fn keeps_agents(&self) -> bool {
        true
    }

    async fn agent_definitions(&self, _: &str) -> adk_surface::Result<Vec<AgentDetail>> {
        Ok(vec![
            detail("finance", "Finance", AgentLife::Active),
            detail("old", "Old", AgentLife::Removed),
        ])
    }

    async fn agent(&self, user: &str, id: &str) -> adk_surface::Result<Option<AgentDetail>> {
        let all = self.agent_definitions(user).await?;
        Ok(all.into_iter().find(|detail| detail.id == id))
    }

    async fn pause_agent(&self, _: &Caller, agent: &str) -> adk_surface::Result<AgentDetail> {
        Ok(detail(agent, "Finance", AgentLife::Paused))
    }
}

struct Fixture {
    source: Arc<dyn Source>,
    service: Arc<dyn ApprovalService>,
    sessions: Arc<dyn SessionService>,
}

async fn fixture() -> Fixture {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "ana".into(),
            session_id: Some("s1".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_clock(Arc::new(ManualClock::new(at(10, 2))))
            .with_router(
                ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
            ),
    );
    let inner = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(Arc::clone(&service))
        .build()
        .unwrap();
    Fixture { source: Arc::new(Home { inner }), service, sessions }
}

/// The engine every test reads, with `hints` in order and `grants`.
fn engine(
    fixture: &Fixture,
    hints: Vec<Arc<dyn CardHints>>,
    grants: &[GrantedUse],
) -> SurfaceEngine {
    let mut builder = SurfaceEngine::builder(Arc::clone(&fixture.source))
        .places([Place::phone(), Place::chat("Telegram")])
        .utc_offset(FixedOffset::east_opt(0).unwrap())
        .clock(Arc::new(ManualClock::new(at(10, 5))))
        .heading(|request| format!("Run {}", request.tool_name));
    for hints in hints {
        builder = builder.card_hints(hints);
    }
    for granted in grants {
        builder = builder.grant(granted.clone());
    }
    builder.build()
}

/// A feature's hint: `hint` for every request whose tool `source` publishes.
struct Fixed {
    source: &'static str,
    hint: CardHint,
}

#[async_trait]
impl CardHints for Fixed {
    async fn hint(&self, request: &ApprovalRequest) -> Option<CardHint> {
        let origin = request.origin.as_ref()?;
        (origin.source == self.source).then(|| self.hint.clone())
    }
}

fn hints(source: &'static str, hint: CardHint) -> Arc<dyn CardHints> {
    Arc::new(Fixed { source, hint })
}

fn call(tool: &str, args: Value, origin: Option<&str>) -> ToolConfirmationRequest {
    ToolConfirmationRequest {
        tool_name: tool.into(),
        function_call_id: Some(format!("call-{tool}")),
        args,
        origin: origin.map(|source| ToolOrigin::new(source, &json!({ "name": tool }), None)),
        acting: None,
    }
}

fn snooze() -> ToolConfirmationRequest {
    call("verb_snooze", json!({ "item": "agenda-7f3", "to": "2026-10-26T19:00:00Z" }), Some(VERBS))
}

/// `call` held in ana's session at 10:02 at `risk`, and the session event that held it.
async fn hold(
    fixture: &Fixture,
    call: &ToolConfirmationRequest,
    risk: ApprovalRisk,
) -> ApprovalRequest {
    let mut request = ApprovalRequest::new(
        format!("apr-{}", call.tool_name),
        ApprovalScope::new(APP, "ana", "s1"),
        call,
        ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
    )
    .with_risk(risk);
    request.created_at = at(10, 2);
    let mut event = Event::new("inv-1");
    event.author = "finance".into();
    event.actions =
        EventActions { tool_confirmation: Some(call.clone()), ..EventActions::default() };
    fixture.sessions.append_event("s1", event).await.unwrap();
    fixture.service.submit(request).await.unwrap()
}

fn web() -> Caller {
    Caller::human("ana", SurfaceKind::Web, Origin::Pointer).with_groups(["owner"])
}

/// One caller per row of the §7.2 table a person answers from.
fn rows() -> Vec<Caller> {
    vec![
        web(),
        Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard)
            .with_groups(["owner"])
            .with_paired_local(),
        Caller::human("ana", SurfaceKind::Phone, Origin::Touch).with_groups(["owner"]),
        Caller::human("ana", SurfaceKind::Tray, Origin::Pointer).with_groups(["owner"]),
        Caller::human("ana", SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() })
            .with_groups(["owner"]),
        Caller::human("ana", SurfaceKind::Web, Origin::Voice).with_groups(["owner"]),
    ]
}

fn decide(request: &ApprovalRequest, answer: Answer) -> Command {
    Command::Decide {
        decision_id: request.id.clone(),
        version: request.version,
        answer,
        edited_args: None,
        reason: None,
        expanded: true,
    }
}

fn answers(card: &DecisionCard) -> Vec<Answer> {
    card.allowed.options.iter().map(|option| option.answer).collect()
}

/// Spec 3.3 §7.1: per field, the first hint that sets it wins; a field no hint sets keeps the
/// engine's own heading and reversibility; a chat card takes the chat heading and never the
/// detail line (S-I5); a request of another origin is drawn as before.
#[tokio::test]
async fn the_first_hint_that_sets_a_field_wins_and_the_engine_stays_the_fallback() {
    let fixture = fixture().await;
    let held = hold(&fixture, &snooze(), ApprovalRisk::Low).await;
    let email = hold(
        &fixture,
        &call("send_email", json!({ "to": "[client email]" }), Some("mcp:mail")),
        ApprovalRisk::Medium,
    )
    .await;
    let detail = CardDetail::new("From Coming up").with_link("Open in Threads", "/ui/threads");
    let first = hints(
        VERBS,
        CardHint::new()
            .with_heading("Move News digest to 19:00")
            .with_first_step("Finance read 1 web page (example.com)"),
    );
    let second = hints(
        VERBS,
        CardHint::new()
            .with_heading("Not this one")
            .with_chat_heading("Snooze something in Coming up")
            .with_reversibility(Reversibility::CanBeChangedBack)
            .with_detail(detail.clone()),
    );
    let plain = engine(&fixture, Vec::new(), &[]);
    let hinted = engine(&fixture, vec![first, second], &[]);
    let chat = &rows()[4];

    let before = plain.decision(&web(), &held.id).await.unwrap();
    assert_eq!(before.heading, "Run verb_snooze", "the engine's heading is the fallback");
    let mut expected = before.clone();
    expected.heading = "Move News digest to 19:00".into();
    expected.timeline.insert(
        0,
        TimelineStep { text: "Finance read 1 web page (example.com)".into(), current: false },
    );
    expected.reversibility = Reversibility::CanBeChangedBack;
    expected.reversibility_text = "You can change it back".into();
    expected.detail = Some(detail);
    assert_eq!(hinted.decision(&web(), &held.id).await.unwrap(), expected);

    let mut expected = plain.decision(chat, &held.id).await.unwrap();
    expected.heading = "Snooze something in Coming up".into();
    expected.timeline.insert(
        0,
        TimelineStep { text: "Finance read 1 web page (example.com)".into(), current: false },
    );
    expected.reversibility = Reversibility::CanBeChangedBack;
    expected.reversibility_text = "You can change it back".into();
    assert_eq!(hinted.decision(chat, &held.id).await.unwrap(), expected);

    for caller in rows() {
        assert_eq!(
            hinted.decision(&caller, &email.id).await.unwrap(),
            plain.decision(&caller, &email.id).await.unwrap(),
            "another origin's card is unchanged"
        );
    }
}

/// Spec 3.3 §7.2: a hint that takes **Always allow** away takes it off every row, and the
/// engine refuses it as an answer, while every other answer stays as it was.
#[tokio::test]
async fn a_hint_without_always_allow_removes_it_on_every_row() {
    let fixture = fixture().await;
    let held = hold(&fixture, &snooze(), ApprovalRisk::Low).await;
    let plain = engine(&fixture, Vec::new(), &[]);
    let hinted = engine(&fixture, vec![hints(VERBS, CardHint::new().without_always_allow())], &[]);

    assert_eq!(
        answers(&plain.decision(&web(), &held.id).await.unwrap()),
        [Answer::AllowOnce, Answer::AlwaysAllow, Answer::Edit, Answer::Deny],
        "without the hint the home offers it"
    );
    for caller in rows() {
        let mut expected = plain.decision(&caller, &held.id).await.unwrap();
        expected.allowed.options.retain(|option| option.answer != Answer::AlwaysAllow);
        let card = hinted.decision(&caller, &held.id).await.unwrap();
        assert_eq!(card, expected, "{:?}", caller.surface);
        assert!(!answers(&card).contains(&Answer::AlwaysAllow));
    }

    let refused = hinted.execute(&web(), decide(&held, Answer::AlwaysAllow)).await.unwrap_err();
    assert!(matches!(refused, SurfaceError::Rule(RuleViolation::NotOffered)), "{refused:?}");
    assert_eq!(fixture.service.get(&held.id).await.unwrap().unwrap(), held, "nothing was decided");
}

/// Spec 3.3 §7.2, §7.4 rule 4: a hint that marks money moves a low-risk card to the high-risk
/// column — "Confirm it's you" on the home and the phone, nothing to approve from the tray,
/// chat or voice, no Always allow — and its risk stays the inbox entry's.
#[tokio::test]
async fn a_hint_that_marks_money_moves_the_card_to_the_high_risk_column() {
    let fixture = fixture().await;
    let low = hold(&fixture, &snooze(), ApprovalRisk::Low).await;
    let high = hold(
        &fixture,
        &call("verb_rearrange", json!({ "item": "agenda-7f3" }), Some("verbs:other")),
        ApprovalRisk::High,
    )
    .await;
    let hinted = engine(&fixture, vec![hints(VERBS, CardHint::new().spends_money())], &[]);

    for caller in rows() {
        let card = hinted.decision(&caller, &low.id).await.unwrap();
        let high_risk = hinted.decision(&caller, &high.id).await.unwrap();
        assert_eq!(card.allowed, high_risk.allowed, "{:?}", caller.surface);
        assert_eq!((card.risk, card.risk_text.as_str(), card.money), (Risk::Low, "Low risk", true));
    }
    let home = hinted.decision(&web(), &low.id).await.unwrap();
    assert!(home.allowed.options.iter().any(|option| option.needs_presence));

    let refused = hinted.execute(&web(), decide(&low, Answer::AllowOnce)).await.unwrap_err();
    assert!(matches!(refused, SurfaceError::Rule(RuleViolation::PresenceRequired)), "{refused:?}");
    let refused = hinted.execute(&rows()[4], decide(&low, Answer::AllowOnce)).await.unwrap_err();
    assert!(matches!(refused, SurfaceError::Rule(RuleViolation::NotOffered)), "{refused:?}");
    assert_eq!(fixture.service.get(&low.id).await.unwrap().unwrap(), low, "nothing was decided");
}

/// Spec 3.3 §7.2: a hint that says money is not spent, or that Always allow is offered,
/// changes nothing, whatever else it says: no answer is added and the risk stays the inbox
/// entry's.
#[tokio::test]
async fn a_hint_never_adds_an_answer_or_changes_the_risk() {
    let fixture = fixture().await;
    let pay = hold(
        &fixture,
        &call("payments_charge", json!({ "cents": 500 }), Some(VERBS)),
        ApprovalRisk::Medium,
    )
    .await;
    let mut quorum = ApprovalRequest::new(
        "apr-quorum",
        ApprovalScope::new(APP, "ana", "s1"),
        &snooze(),
        ApprovalPolicy::new([ApproverSelector::group("owner")])
            .allow_self_approval()
            .with_quorum(2),
    )
    .with_risk(ApprovalRisk::Low);
    quorum.created_at = at(10, 3);
    let quorum = fixture.service.submit(quorum).await.unwrap();
    let permissive = CardHint {
        money: Some(false),
        offer_always: Some(true),
        ..CardHint::new()
            .with_heading("Harmless")
            .with_reversibility(Reversibility::CanBeChangedBack)
    };
    let plain = engine(&fixture, Vec::new(), &[]);
    let hinted = engine(&fixture, vec![hints(VERBS, permissive)], &[]);

    for caller in rows() {
        for request in [&pay, &quorum] {
            let before = plain.decision(&caller, &request.id).await.unwrap();
            let after = hinted.decision(&caller, &request.id).await.unwrap();
            assert_eq!(
                (&after.allowed, after.risk, &after.risk_text, after.money),
                (&before.allowed, before.risk, &before.risk_text, before.money),
                "{:?} {}",
                caller.surface,
                request.tool_name
            );
        }
    }
    let pay_card = hinted.decision(&web(), &pay.id).await.unwrap();
    assert!(pay_card.money, "the engine's money test still counts");
    assert!(
        !answers(&hinted.decision(&web(), &quorum.id).await.unwrap())
            .contains(&Answer::AlwaysAllow)
    );
}

/// Spec 3.3 §7.1: every card the engine draws carries the hint — the open decisions, one
/// session's, one decision, the feed's, and one decided since.
#[tokio::test]
async fn every_card_the_engine_draws_carries_the_hint() {
    let fixture = fixture().await;
    let held = hold(&fixture, &snooze(), ApprovalRisk::Low).await;
    let hinted = engine(
        &fixture,
        vec![hints(VERBS, CardHint::new().with_heading("Move News digest to 19:00"))],
        &[],
    );
    let heading = |cards: Vec<DecisionCard>| -> Vec<String> {
        cards.into_iter().map(|card| card.heading).collect()
    };
    let expected = vec!["Move News digest to 19:00".to_string()];

    assert_eq!(heading(hinted.decisions(&web()).await.unwrap()), expected);
    assert_eq!(heading(hinted.session_decisions(&web(), "s1").await.unwrap()), expected);
    assert_eq!(heading(vec![hinted.decision(&web(), &held.id).await.unwrap()]), expected);
    let mut feed = hinted.feed(web(), "s1".into(), None);
    let open = loop {
        match feed.next().await.expect("the feed ended").unwrap() {
            FeedFrame::Decisions { cards } => break cards,
            FeedFrame::Entry { .. } | FeedFrame::Reset { .. } => {}
        }
    };
    assert_eq!(heading(open), expected);

    let outcome = hinted.execute(&web(), decide(&held, Answer::Deny)).await.unwrap();
    let CommandOutcome::Answered { card } = outcome else { panic!("{outcome:?}") };
    assert_eq!(heading(vec![card]), expected);
    assert_eq!(heading(hinted.decided_since(&web(), at(10, 0)).await.unwrap()), expected);
}

/// Spec 3.3 §5.7, S-I12, S-I13: an agent the home runs, added or paused, lists what the home
/// grants every agent it runs, in the order granted, a second grant of one id replacing the
/// first; a removed agent lists nothing, and a home that grants nothing lists nothing.
#[tokio::test]
async fn an_agent_that_runs_lists_what_the_home_grants() {
    let fixture = fixture().await;
    let draws =
        GrantedUse::new("genui", "Show forms and tables in your threads — sending asks you first");
    let verbs =
        GrantedUse::new("verb-ui", "Ask to change things on your screens — always asks you first");
    let home = engine(
        &fixture,
        Vec::new(),
        &[GrantedUse::new("genui", "Show forms"), verbs.clone(), draws.clone()],
    );
    let granted = vec![draws, verbs];
    assert_eq!(home.granted(), granted);

    let mut finance = detail("finance", "Finance", AgentLife::Active);
    finance.granted = granted.clone();
    assert_eq!(home.agent(&web(), "finance").await.unwrap(), finance);
    let old = detail("old", "Old", AgentLife::Removed);
    assert_eq!(home.agent(&web(), "old").await.unwrap(), old);
    assert_eq!(home.agent_details(&web()).await.unwrap(), vec![finance, old]);

    let mut paused = detail("finance", "Finance", AgentLife::Paused);
    paused.granted = granted;
    let outcome =
        home.execute(&web(), Command::PauseAgent { agent: "finance".into() }).await.unwrap();
    assert_eq!(outcome, CommandOutcome::AgentPaused { agent: paused });

    let bare = engine(&fixture, Vec::new(), &[]);
    assert_eq!(
        bare.agent(&web(), "finance").await.unwrap(),
        detail("finance", "Finance", AgentLife::Active)
    );
}
