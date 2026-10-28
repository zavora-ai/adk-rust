//! Drawings in the thread (spec 3.3 §6.1 `genui`, §7.4 rule 5): only a render tool's result
//! that writes the reserved state key with its drawing's digest is drawn (provenance), every
//! drawing is checked again as it is projected, its strings come resolved, and a sent form's
//! state follows from its decision and its call's result.

#![cfg(feature = "genui")]

use std::sync::Arc;

use adk_core::{
    Content, Event, FunctionResponseData, InMemoryApprovalStore, Part, StoreApprovalService,
};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::genui::{
    A2uiMessage, DRAWN_STATE_KEY, DrawnEnvelope, DrawnInput, DrawnKind, DrawnNode, DrawnRecord,
    DrawnSent, FORMS_CATALOG_ID, FormStatus, RefusalKind, form_states,
};
use adk_surface::{
    Caller, DecidedAnswer, FeedEntry, LocalSource, Origin, SurfaceEngine, SurfaceKind, ThreadItem,
};
use chrono::{TimeZone, Utc};
use proptest::prelude::*;
use serde_json::{Value, json};

const APP: &str = "home";

async fn engine() -> (SurfaceEngine, Arc<dyn SessionService>) {
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
    let source = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))))
        .build()
        .unwrap();
    let engine = SurfaceEngine::builder(Arc::new(source))
        .utc_offset(chrono::FixedOffset::east_opt(0).unwrap())
        .build();
    (engine, sessions)
}

fn ana() -> Caller {
    Caller::human("ana", SurfaceKind::Web, Origin::Pointer)
}

/// The envelope of a card whose only text is `text`.
fn card(text: &str) -> DrawnEnvelope {
    envelope(
        "status",
        DrawnKind::Card,
        vec![json!({ "id": "root", "component": "Text", "text": text })],
        json!({}),
    )
}

fn envelope(surface: &str, kind: DrawnKind, components: Vec<Value>, model: Value) -> DrawnEnvelope {
    DrawnEnvelope {
        surface_id: surface.into(),
        version: 1,
        kind,
        catalog_id: FORMS_CATALOG_ID.into(),
        title: None,
        messages: vec![
            A2uiMessage::create_surface(surface),
            A2uiMessage::update_components(surface, components),
            A2uiMessage::update_data_model(surface, None, Some(model)),
        ],
        actions: Vec::new(),
        deleted: false,
    }
}

/// The event of `tool`'s result `envelope`, with the reserved state key `record` when given.
fn result(tool: &str, envelope: &DrawnEnvelope, record: Option<DrawnRecord>) -> Event {
    let mut event = Event::new("inv-1");
    event.author = "finance".into();
    event.llm_response.content = Some(Content {
        role: "function".into(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData::new(
                tool,
                json!({ "status": "shown", "drawn": envelope }),
            ),
            id: Some("call-1".into()),
            annotations: None,
        }],
    });
    if let Some(record) = record {
        event.actions.state_delta.insert(DRAWN_STATE_KEY.into(), json!(record));
    }
    event
}

fn record_of(envelope: &DrawnEnvelope) -> DrawnRecord {
    DrawnRecord {
        surface_id: envelope.surface_id.clone(),
        version: envelope.version,
        digest: envelope.digest(),
    }
}

async fn items_of(events: Vec<Event>) -> Vec<ThreadItem> {
    let (engine, sessions) = engine().await;
    for event in events {
        sessions.append_event("s1", event).await.unwrap();
    }
    engine.thread(&ana(), "s1").await.unwrap().into_iter().flat_map(|entry| entry.items).collect()
}

/// Provenance: the same envelope is a drawing only with the reserved key and its digest; an
/// MCP server's `render_form`, or a forged digest, is an ordinary result.
#[tokio::test]
async fn only_a_render_tools_own_result_is_drawn() {
    let shown = card("Invoice sent to ACME");
    let drawn = items_of(vec![result("render_card", &shown, Some(record_of(&shown)))]).await;
    assert!(matches!(drawn.as_slice(), [ThreadItem::Drawn { .. }]), "{drawn:?}");

    let without = items_of(vec![result("render_card", &shown, None)]).await;
    assert!(
        matches!(without.as_slice(), [ThreadItem::Result { tool, ok: true, .. }] if tool == "render_card"),
        "{without:?}"
    );

    let mut forged = record_of(&shown);
    forged.digest = card("Something else").digest();
    let forged = items_of(vec![result("render_card", &shown, Some(forged))]).await;
    assert!(matches!(forged.as_slice(), [ThreadItem::Result { .. }]), "{forged:?}");

    // A tool of another name never draws, even with the key.
    let other = items_of(vec![result("show_card", &shown, Some(record_of(&shown)))]).await;
    assert!(matches!(other.as_slice(), [ThreadItem::Result { .. }]), "{other:?}");
}

/// A table's rows come expanded from their template, their strings resolved, with "Show all"
/// past ten; a form's field carries its path, its input and the value it starts with.
#[tokio::test]
async fn strings_come_resolved_and_templates_expanded() {
    let rows: Vec<Value> =
        (1..=12).map(|day| json!([format!("2026-10-{day:02}"), "Client call"])).collect();
    let table = envelope(
        "hours",
        DrawnKind::Table,
        vec![
            json!({ "id": "root", "component": "Card", "child": "body" }),
            json!({ "id": "body", "component": "Column", "children": ["rows"] }),
            json!({ "id": "rows", "component": "List", "children": { "componentId": "row", "path": "/rows" } }),
            json!({ "id": "row", "component": "Row", "children": ["date", "task"] }),
            json!({ "id": "date", "component": "Text", "text": { "path": "0" } }),
            json!({ "id": "task", "component": "Text", "text": { "path": "1" } }),
        ],
        json!({ "rows": rows }),
    );
    let items = items_of(vec![result("render_table", &table, Some(record_of(&table)))]).await;
    let [ThreadItem::Drawn { surface }] = items.as_slice() else { panic!("{items:?}") };
    let list = &surface.root.as_ref().unwrap().children[0].children[0];
    assert_eq!(list.component, "List");
    assert_eq!(list.children.len(), 12);
    assert_eq!(list.show_all_text.as_deref(), Some("Show all 12"));
    let first: Vec<Option<&str>> =
        list.children[0].children.iter().map(|cell| cell.text.as_deref()).collect();
    assert_eq!(first, [Some("2026-10-01"), Some("Client call")]);
    assert_eq!(list.children[3].children[0].key, "date@/rows/3");

    let form = envelope(
        "invoice",
        DrawnKind::Screen,
        vec![
            json!({ "id": "root", "component": "Column", "children": ["to"] }),
            json!({ "id": "to", "component": "TextField", "label": "To", "value": { "path": "/to" },
                    "checks": [{ "condition": { "call": "email", "args": { "value": { "path": "/to" } } },
                                 "message": "Enter an email address" }] }),
        ],
        json!({ "to": "ana@example.com" }),
    );
    let items = items_of(vec![result("render_screen", &form, Some(record_of(&form)))]).await;
    let [ThreadItem::Drawn { surface }] = items.as_slice() else { panic!("{items:?}") };
    let field: &DrawnNode = &surface.root.as_ref().unwrap().children[0];
    assert_eq!(field.text.as_deref(), Some("To"));
    let input = field.field.as_ref().unwrap();
    assert_eq!(
        (input.path.as_str(), input.input, &input.value, input.checks[0].message.as_str()),
        ("/to", DrawnInput::ShortText, &json!("ana@example.com"), "Enter an email address")
    );
}

/// A drawing whose record checks out but which rule 5 refuses — written before a stricter
/// check, or forged — is the plain notice, never a drawing.
#[tokio::test]
async fn a_recorded_drawing_is_checked_again() {
    let shown = card("Approved");
    let items = items_of(vec![result("render_card", &shown, Some(record_of(&shown)))]).await;
    let [ThreadItem::DrawnRefused { refusal }] = items.as_slice() else { panic!("{items:?}") };
    assert_eq!(refusal.kind, RefusalKind::LooksLikeDecision);
    assert_eq!(
        refusal.text,
        "Finance tried to show something that looks like a decision. It wasn't shown."
    );
    assert!(refusal.reason.starts_with("catalog forms 1.0 · component root Text · "));
}

fn entry(seq: u64, items: Vec<ThreadItem>) -> FeedEntry {
    FeedEntry {
        seq,
        at: Utc.with_ymd_and_hms(2026, 10, 27, 10, 3 + seq as u32, 0).unwrap(),
        at_text: format!("10:{:02}", 3 + seq),
        items,
    }
}

fn sent(held: bool) -> ThreadItem {
    ThreadItem::DrawnSent {
        sent: DrawnSent {
            surface_id: "invoice-10".into(),
            version: 1,
            action_id: "send".into(),
            tool: "send_invoice".into(),
            call_id: "adk-call-requested-1".into(),
            decision_id: held.then(|| "apr-1".to_string()),
            held,
            at: Utc.with_ymd_and_hms(2026, 10, 27, 10, 3, 0).unwrap(),
            text: if held {
                "Sent at 10:03".into()
            } else {
                "Finance can't use send_invoice here".into()
            },
            via: adk_surface::genui::SentVia::Form,
        },
    }
}

fn result_item(ok: bool, text: &str) -> ThreadItem {
    ThreadItem::Result {
        tool: "send_invoice".into(),
        call_id: Some("adk-call-requested-1".into()),
        ok,
        text: text.into(),
    }
}

/// Spec 3.3 §7.4 rule 5: a form's state comes from its decision and its call's result.
#[test]
fn a_sent_forms_state_follows_its_decision_and_its_result() {
    let state = |entries: &[FeedEntry], answer: Option<DecidedAnswer>| {
        let states = form_states(entries, |_| answer);
        assert_eq!(states.len(), 1, "{states:?}");
        (states[0].status, states[0].text.clone(), states[0].locked)
    };
    let waiting = [entry(0, vec![sent(true)])];
    assert_eq!(
        state(&waiting, None),
        (FormStatus::Waiting, "Sent at 10:03 · waiting for you".into(), true)
    );
    assert_eq!(
        state(&waiting, Some(DecidedAnswer::Yes)),
        (FormStatus::Sent, "Sent at 10:03".into(), true)
    );
    assert_eq!(
        state(&waiting, Some(DecidedAnswer::No)),
        (FormStatus::SaidNo, "Sent · you said no".into(), true)
    );
    assert_eq!(
        state(&waiting, Some(DecidedAnswer::Ended)),
        (FormStatus::Ended, "Ended without an answer".into(), true)
    );
    let done = [entry(0, vec![sent(true)]), entry(1, vec![result_item(true, "Done")])];
    assert_eq!(
        state(&done, Some(DecidedAnswer::Yes)),
        (FormStatus::Done, "Sent · done at 10:04".into(), true)
    );
    let failed = [entry(0, vec![sent(true)]), entry(1, vec![result_item(false, "Failed")])];
    assert_eq!(
        state(&failed, Some(DecidedAnswer::Yes)),
        (FormStatus::Failed, "Sent · failed".into(), true)
    );
    assert_eq!(
        state(&failed, Some(DecidedAnswer::No)),
        (FormStatus::SaidNo, "Sent · you said no".into(), true)
    );
    let not_run = [
        entry(0, vec![sent(true)]),
        entry(1, vec![result_item(false, "Not run: Finance can't use send_invoice any more")]),
    ];
    assert_eq!(
        state(&not_run, Some(DecidedAnswer::Yes)),
        (FormStatus::NotRun, "Not run: Finance can't use send_invoice any more".into(), true)
    );
    let refused = [entry(0, vec![sent(false)])];
    assert_eq!(
        state(&refused, None),
        (FormStatus::Refused, "Finance can't use send_invoice here".into(), false)
    );
}

/// Each reserved wording, in a sentence of other words.
fn with_reserved() -> impl Strategy<Value = String> {
    (
        "[A-Za-z ]{0,12}",
        proptest::sample::select(adk_surface::genui::RESERVED_PHRASES.to_vec()),
        "[A-Za-z ]{0,12}",
    )
        .prop_map(|(before, phrase, after)| format!("{before} {phrase} {after}"))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// A reserved word, or a component outside the catalog, never becomes a drawing, even
    /// with a record that checks out (spec 3.3 §7.4 rule 5).
    #[test]
    fn a_reserved_word_or_an_off_catalog_component_is_never_drawn(
        text in with_reserved(),
        component in "(Image|Icon|Modal|Video|AudioPlayer|[A-Z][a-z]{3,9}Widget)",
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let reserved = card(&text);
        let off_catalog = envelope(
            "picture",
            DrawnKind::Card,
            vec![json!({ "id": "root", "component": component, "text": "ok" })],
            json!({}),
        );
        let items = runtime.block_on(items_of(vec![
            result("render_card", &reserved, Some(record_of(&reserved))),
            result("render_screen", &off_catalog, Some(record_of(&off_catalog))),
        ]));
        prop_assert!(!items.iter().any(|item| matches!(item, ThreadItem::Drawn { .. })), "{items:?}");
        prop_assert_eq!(
            items.iter().filter(|item| matches!(item, ThreadItem::DrawnRefused { .. })).count(),
            2
        );
    }
}
