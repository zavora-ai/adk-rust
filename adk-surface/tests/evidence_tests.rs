//! Approve with the image through the engine and the web API (spec 3.7 §8.2, X11): a held
//! call's images reach every card from its inbox entry, the image route serves only the exact
//! bytes of the digest to a viewer who may see the decision, an answer that lets the call run
//! names the digests it saw, and each surface answers as the boards decided — Deny only when
//! nothing can be sent, by voice, in chat and from the tray.

#![cfg(feature = "web")]

use std::sync::Arc;

use adk_artifact::{ArtifactService, DeleteRequest, InMemoryArtifactService, SaveRequest};
use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalScope, ApprovalService, ApprovalState,
    ApproverSelector, ArtifactLocator, Event, EventActions, Evidence, ImageEvidence,
    InMemoryApprovalStore, ManualClock, Part, StoreApprovalService, ToolConfirmationRequest,
};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::{
    Answer, Caller, CardHint, CardHints, Command, CommandOutcome, ImageChange, LocalSource,
    MediaFacts, MediaNoticeKind, Origin, Place, SurfaceEngine, SurfaceError, SurfaceKind,
};
use async_trait::async_trait;
use chrono::{DateTime, Duration, FixedOffset, TimeZone, Utc};
use serde_json::{Value, json};
use sha2::Digest as _;

const APP: &str = "desk";

fn at(hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 18, hour, minute, 0).unwrap()
}

/// A 40 × 20 PNG, one colour, and its digest.
fn png(color: [u8; 4]) -> (Vec<u8>, String) {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 40, 20);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let pixels: Vec<u8> = (0..40 * 20).flat_map(|_| color).collect();
        encoder.write_header().unwrap().write_image_data(&pixels).unwrap();
    }
    let digest = format!("sha256:{:x}", sha2::Sha256::digest(&bytes));
    (bytes, digest)
}

struct Fixture {
    engine: SurfaceEngine,
    service: Arc<dyn ApprovalService>,
    sessions: Arc<dyn SessionService>,
    artifacts: Arc<InMemoryArtifactService>,
}

/// Where the images go and what they cost, as the vision pack would say.
struct Vision;

#[async_trait]
impl CardHints for Vision {
    async fn hint(&self, request: &ApprovalRequest) -> Option<CardHint> {
        (request.tool_name == "vision_describe").then(|| {
            CardHint::new().with_heading("Send this image to Claude").with_media(
                MediaFacts::new()
                    .to("Claude", "claude-sonnet-5-5", ["api.anthropic.com"])
                    .with_data_class("document_crop")
                    .with_estimated_cost(4_400, Some("851 image tokens".into()))
                    .with_cut_from("id-card.png"),
            )
        })
    }
}

async fn fixture(now: DateTime<Utc>) -> Fixture {
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
            .with_clock(Arc::new(ManualClock::new(at(10, 14))))
            .with_router(
                ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
            ),
    );
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let source = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(Arc::clone(&service))
        .artifacts(artifacts.clone() as Arc<dyn ArtifactService>)
        .build()
        .unwrap();
    let engine = SurfaceEngine::builder(Arc::new(source))
        .places([Place::web(), Place::phone(), Place::chat("Telegram")])
        .utc_offset(FixedOffset::east_opt(0).unwrap())
        .clock(Arc::new(ManualClock::new(now)))
        .card_hints(Arc::new(Vision))
        .build();
    Fixture { engine, service, sessions, artifacts }
}

/// Stores `bytes` as the egress copy and holds a `vision_describe` call with it as evidence,
/// its regions `regions`.
async fn hold(fixture: &Fixture, bytes: &[u8], digest: &str, regions: Value) -> ApprovalRequest {
    let saved = fixture
        .artifacts
        .save(SaveRequest {
            app_name: APP.into(),
            user_id: "ana".into(),
            session_id: "s1".into(),
            file_name: "vision-egress.png".into(),
            part: Part::InlineData {
                mime_type: "image/png".into(),
                data: bytes.to_vec(),
                uri: None,
                annotations: None,
            },
            version: None,
        })
        .await
        .unwrap();
    let evidence = ImageEvidence::new(
        digest,
        ArtifactLocator::new(APP, "ana", "s1", "vision-egress.png", saved.version),
        "image/png",
        40,
        20,
    )
    .with_regions(regions);
    let call = ToolConfirmationRequest::new(
        "vision_describe",
        json!({ "image": "id-card.png", "backend": "claude", "question": "What kind of document is this?" }),
    )
    .with_evidence([Evidence::from(evidence)]);
    let mut request = ApprovalRequest::new(
        "dec-7c41e0",
        ApprovalScope::new(APP, "ana", "s1"),
        &call,
        ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
    );
    request.created_at = at(10, 14);
    let mut event = Event::new("inv-1");
    event.author = "desk".into();
    event.actions = EventActions { tool_confirmation: Some(call), ..EventActions::default() };
    fixture.sessions.append_event("s1", event).await.unwrap();
    fixture.service.submit(request).await.unwrap()
}

fn face_and_text() -> Value {
    json!({ "regions": [
        { "label": "faces", "shape": { "kind": "box", "x0": 0, "y0": 0, "x1": 10, "y1": 20 }, "origin": {} },
        { "label": "personal_text", "shape": { "kind": "box", "x0": 14, "y0": 2, "x1": 38, "y1": 8 }, "origin": {} },
    ] })
}

fn everything() -> Value {
    json!({ "regions": [
        { "label": "schema_personal_fields", "shape": { "kind": "box", "x0": 0, "y0": 0, "x1": 40, "y1": 10 }, "origin": {} },
        { "label": "schema_personal_fields", "shape": { "kind": "box", "x0": 0, "y0": 10, "x1": 40, "y1": 20 }, "origin": {} },
    ] })
}

fn caller(surface: SurfaceKind, origin: Origin) -> Caller {
    Caller::human("ana", surface, origin).with_groups(["owner"])
}

fn web() -> Caller {
    caller(SurfaceKind::Web, Origin::Pointer)
}

fn answers(card: &adk_surface::DecisionCard) -> Vec<Answer> {
    card.allowed.options.iter().map(|option| option.answer).collect()
}

fn decide(card: &adk_surface::DecisionCard, answer: Answer, evidence: Vec<String>) -> Command {
    Command::Decide {
        decision_id: card.id.clone(),
        version: card.version,
        answer,
        edited_args: None,
        reason: None,
        expanded: true,
        evidence,
    }
}

#[tokio::test]
async fn the_card_shows_the_exact_image_with_what_was_covered_and_where_it_goes() {
    let fixture = fixture(at(10, 15)).await;
    let (bytes, digest) = png([200, 220, 240, 255]);
    hold(&fixture, &bytes, &digest, face_and_text()).await;

    let card = fixture.engine.decision(&web(), "dec-7c41e0").await.unwrap();
    let media = card.media.as_ref().expect("the card carries its image");
    assert_eq!(card.heading, "Send this image to Claude");
    assert_eq!(media.title, "The exact image that would leave");
    assert_eq!(media.images[0].digest, digest);
    assert_eq!(media.images[0].size_text, "40 × 20 PNG");
    assert_eq!(media.images[0].regions.len(), 2);
    let covered: Vec<&str> = media.covered.iter().map(|line| line.text.as_str()).collect();
    assert_eq!(covered, ["1 face", "1 personal text"]);
    assert_eq!(media.goes_to.as_ref().unwrap().host, "api.anthropic.com");
    assert_eq!(media.what.as_deref(), Some("Document crop"));
    assert_eq!(media.cost_text.as_deref(), Some("Up to $0.0044"));
    assert_eq!(media.kept_text.as_deref(), Some("Kept as evidence until 18 Dec"));
    assert!(media.notice.is_none());
    // Allow once, Edit and Deny; never Always allow, which the egress rules decide.
    assert_eq!(answers(&card), [Answer::AllowOnce, Answer::Deny]);

    // The image route serves exactly the bytes of the digest.
    let served = fixture.engine.evidence_image(&web(), "dec-7c41e0", 0).await.unwrap();
    assert_eq!(served.bytes, bytes);
    assert_eq!(served.mime_type, "image/png");
    // Another person never sees it.
    let other = Caller::human("bo", SurfaceKind::Web, Origin::Pointer).with_groups(["owner"]);
    assert!(matches!(
        fixture.engine.evidence_image(&other, "dec-7c41e0", 0).await,
        Err(SurfaceError::NotFound { .. })
    ));
}

#[tokio::test]
async fn an_answer_that_sends_the_image_names_the_digest_it_saw() {
    let fixture = fixture(at(10, 15)).await;
    let (bytes, digest) = png([10, 20, 30, 255]);
    hold(&fixture, &bytes, &digest, face_and_text()).await;
    let card = fixture.engine.decision(&web(), "dec-7c41e0").await.unwrap();

    // No digest: refused, nothing decided.
    let bare = fixture.engine.execute(&web(), decide(&card, Answer::AllowOnce, Vec::new())).await;
    assert!(matches!(bare, Err(SurfaceError::Invalid(_))), "{bare:?}");
    // Another digest: the card is shown again, nothing decided.
    let wrong = format!("sha256:{}", "0".repeat(64));
    let other = fixture
        .engine
        .execute(&web(), decide(&card, Answer::AllowOnce, vec![wrong]))
        .await
        .unwrap();
    assert!(matches!(other, CommandOutcome::Changed { .. }), "{other:?}");
    assert_eq!(
        fixture.service.get("dec-7c41e0").await.unwrap().unwrap().state,
        ApprovalState::Pending
    );
    // The digest shown: allowed.
    let given = fixture
        .engine
        .execute(&web(), decide(&card, Answer::AllowOnce, vec![digest.clone()]))
        .await
        .unwrap();
    let CommandOutcome::Answered { card } = given else { panic!("{given:?}") };
    assert_eq!(card.media.unwrap().title, "The image that left");
    assert_eq!(
        fixture.service.get("dec-7c41e0").await.unwrap().unwrap().state,
        ApprovalState::Approved
    );
}

#[tokio::test]
async fn voice_chat_and_the_tray_cannot_say_yes_to_an_image() {
    let fixture = fixture(at(10, 15)).await;
    let (bytes, digest) = png([10, 20, 30, 255]);
    hold(&fixture, &bytes, &digest, face_and_text()).await;
    for caller in [
        caller(SurfaceKind::Voice, Origin::Voice),
        caller(SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() }),
        caller(SurfaceKind::Tray, Origin::Pointer),
    ] {
        let card = fixture.engine.decision(&caller, "dec-7c41e0").await.unwrap();
        assert_eq!(answers(&card), [Answer::Deny], "{:?}", caller.surface);
        assert!(card.allowed.note.is_some());
    }
    // A chat card carries no image and no arguments (S-I5); the phone and terminal show it.
    let chat = caller(SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() });
    assert!(fixture.engine.decision(&chat, "dec-7c41e0").await.unwrap().media.is_none());
    for caller in
        [caller(SurfaceKind::Phone, Origin::Touch), caller(SurfaceKind::Terminal, Origin::Keyboard)]
    {
        let card = fixture.engine.decision(&caller, "dec-7c41e0").await.unwrap();
        assert!(card.media.is_some());
        assert!(answers(&card).contains(&Answer::AllowOnce), "{:?}", caller.surface);
    }
}

#[tokio::test]
async fn a_crop_covered_in_full_has_nothing_to_allow() {
    let fixture = fixture(at(10, 15)).await;
    let (bytes, digest) = png([0, 0, 0, 255]);
    hold(&fixture, &bytes, &digest, everything()).await;
    let card = fixture.engine.decision(&web(), "dec-7c41e0").await.unwrap();
    let media = card.media.as_ref().unwrap();
    assert!(media.covered_all);
    assert_eq!(media.notice.as_ref().unwrap().kind, MediaNoticeKind::AllCovered);
    assert_eq!(answers(&card), [Answer::Deny]);
    let refused =
        fixture.engine.execute(&web(), decide(&card, Answer::AllowOnce, vec![digest])).await;
    assert!(refused.is_err(), "{refused:?}");
    let denied = fixture.engine.execute(&web(), decide(&card, Answer::Deny, Vec::new())).await;
    assert!(matches!(denied, Ok(CommandOutcome::Answered { .. })), "{denied:?}");
}

#[tokio::test]
async fn a_deleted_image_is_shown_as_deleted_and_only_deny_works() {
    let fixture = fixture(at(10, 14) + Duration::days(31)).await;
    let (bytes, digest) = png([10, 20, 30, 255]);
    hold(&fixture, &bytes, &digest, face_and_text()).await;
    fixture
        .artifacts
        .delete(DeleteRequest {
            app_name: APP.into(),
            user_id: "ana".into(),
            session_id: "s1".into(),
            file_name: "vision-egress.png".into(),
            version: None,
        })
        .await
        .unwrap();
    let card = fixture.engine.decision(&web(), "dec-7c41e0").await.unwrap();
    let media = card.media.as_ref().unwrap();
    let image = &media.images[0];
    assert!(image.href.is_none());
    assert_eq!(image.purged.as_ref().unwrap().title, "This image was deleted on 18 Dec");
    assert_eq!(media.notice.as_ref().unwrap().kind, MediaNoticeKind::Purged);
    assert_eq!(answers(&card), [Answer::Deny]);
    assert!(card.timeline.iter().any(|step| step.text == "Image deleted on 18 Dec"));
    assert!(matches!(
        fixture.engine.evidence_image(&web(), "dec-7c41e0", 0).await,
        Err(SurfaceError::NotFound { .. })
    ));
}

#[tokio::test]
async fn a_tampered_image_is_never_served() {
    let fixture = fixture(at(10, 15)).await;
    let (bytes, _) = png([10, 20, 30, 255]);
    let (_, other) = png([90, 20, 30, 255]);
    hold(&fixture, &bytes, &other, face_and_text()).await;
    let served = fixture.engine.evidence_image(&web(), "dec-7c41e0", 0).await;
    assert!(matches!(served, Err(SurfaceError::Invalid(_))), "{served:?}");
}

#[tokio::test]
async fn a_change_is_previewed_but_needs_a_source_that_holds_again() {
    let fixture = fixture(at(10, 15)).await;
    let (bytes, digest) = png([200, 220, 240, 255]);
    hold(&fixture, &bytes, &digest, face_and_text()).await;
    let card = fixture.engine.decision(&web(), "dec-7c41e0").await.unwrap();
    // This source runs no agent, so nothing can hold the call again: the card offers no change.
    assert!(!card.media.as_ref().unwrap().can_change);
    let change = ImageChange { image: 0, crop: Some([0, 0, 40, 12]), cover: vec![[20, 0, 30, 6]] };
    let refused = fixture
        .engine
        .execute(
            &web(),
            Command::ChangeImage {
                decision_id: card.id.clone(),
                version: card.version,
                from: digest.clone(),
                change: change.clone(),
                preview: false,
            },
        )
        .await;
    assert!(matches!(refused, Err(SurfaceError::Unsupported { .. })), "{refused:?}");
    // An agent never changes an image.
    let agent = Caller::human("ana", SurfaceKind::Web, Origin::Agent { agent: "desk".into() })
        .with_groups(["owner"]);
    let by_agent = fixture
        .engine
        .execute(
            &agent,
            Command::ChangeImage {
                decision_id: card.id,
                version: card.version,
                from: digest,
                change,
                preview: true,
            },
        )
        .await;
    assert!(matches!(by_agent, Err(SurfaceError::NotHere(_))), "{by_agent:?}");
}
