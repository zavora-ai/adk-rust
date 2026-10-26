//! Rule 5 (spec 3.3 §7.4 rule 5): agent-drawn UI never contains approval controls, never
//! asks for a secret and shows only the catalog. One test per rule, then properties.

#![cfg(feature = "genui")]

use adk_surface::genui::{
    A2uiMessage, ActionArg, ComponentRef, DrawRefusal, DrawnAction, DrawnKind, FieldArg,
    RESERVED_LABELS, RESERVED_PHRASES, RefusalKind, SurfaceDraft, Validator, decode_jsonl,
    encode_jsonl,
};
use proptest::prelude::*;
use serde_json::{Value, json};

fn text(id: &str, text: Value) -> Value {
    json!({ "id": id, "component": "Text", "text": text })
}

fn screen(components: Vec<Value>, data_model: Value, actions: Vec<DrawnAction>) -> SurfaceDraft {
    SurfaceDraft {
        surface_id: "s".into(),
        kind: DrawnKind::Screen,
        title: None,
        components,
        data_model,
        actions,
    }
}

/// A screen whose root column shows `children`.
fn column(children: Vec<Value>) -> Vec<Value> {
    let ids: Vec<Value> = children.iter().map(|child| child["id"].clone()).collect();
    let mut components = vec![json!({ "id": "root", "component": "Column", "children": ids })];
    components.extend(children);
    components
}

fn send_invoice() -> DrawnAction {
    DrawnAction {
        id: "send".into(),
        label: "Send".into(),
        tool: "send_invoice".into(),
        args: [
            ("to".to_string(), ActionArg::Field(FieldArg { path: "/to".into() })),
            ("invoice".to_string(), ActionArg::Constant(json!("INV-10"))),
        ]
        .into(),
    }
}

/// The invoice form: a field `to` and a Send button for `send_invoice`.
fn invoice_form() -> SurfaceDraft {
    screen(
        vec![
            json!({ "id": "root", "component": "Column", "children": ["to", "send-button"] }),
            json!({ "id": "to", "component": "TextField", "label": "To", "value": { "path": "/to" } }),
            text("send-label", json!("Send")),
            json!({
                "id": "send-button", "component": "Button", "child": "send-label",
                "action": { "event": { "name": "send" } }
            }),
        ],
        json!({ "to": "" }),
        vec![send_invoice()],
    )
}

fn refusal(kind: RefusalKind, component: Option<(&str, &str)>, detail: &str) -> DrawRefusal {
    DrawRefusal {
        kind,
        catalog: "forms 1.0".into(),
        component: component
            .map(|(id, name)| ComponentRef { id: id.into(), component: name.into() }),
        detail: detail.into(),
    }
}

fn refused(draft: &SurfaceDraft) -> DrawRefusal {
    Validator::forms().validate(draft).expect_err("the drawing is refused")
}

#[test]
fn the_invoice_form_passes_and_its_strings_are_resolved() {
    let checked = Validator::forms().validate(&invoice_form()).unwrap();
    let shown: Vec<(&str, &str)> =
        checked.texts.iter().map(|text| (text.field.as_str(), text.text.as_str())).collect();
    assert_eq!(
        shown,
        vec![("label", "To"), ("value", ""), ("text", "Send"), ("actions/send/label", "Send")]
    );
}

#[test]
fn a_reserved_label_is_refused_with_its_technical_reason() {
    let mut draft = invoice_form();
    draft.components[2] = text("send-label", json!("Allow once"));
    assert_eq!(
        refused(&draft),
        refusal(
            RefusalKind::LooksLikeDecision,
            Some(("send-label", "Text")),
            "text matches reserved word \"Allow once\" after normalisation"
        )
    );
}

#[test]
fn look_alike_and_invisible_characters_do_not_hide_a_reserved_word() {
    for disguised in [
        "\u{0410}llow \u{043E}nce",
        "Al\u{200B}low once",
        "\u{FF21}llow once",
        "ALLOW\u{00A0}ONCE",
        "A\u{0301}llow once",
        "Allow\u{2066} once\u{2069}",
    ] {
        let draft = screen(vec![text("root", json!(disguised))], json!({}), Vec::new());
        assert_eq!(refused(&draft).kind, RefusalKind::LooksLikeDecision, "{disguised:?}");
    }
}

#[test]
fn a_reserved_word_through_a_binding_or_format_string_is_refused() {
    let bound = screen(
        vec![text("root", json!({ "path": "/status" }))],
        json!({ "status": "Approved" }),
        Vec::new(),
    );
    assert_eq!(
        refused(&bound),
        refusal(
            RefusalKind::LooksLikeDecision,
            Some(("root", "Text")),
            "text matches reserved word \"Approved\" after normalisation"
        )
    );
    let formatted = screen(
        vec![text(
            "root",
            json!({ "call": "formatString", "args": { "value": "Allow ${/how}" }, "returnType": "string" }),
        )],
        json!({ "how": "once" }),
        Vec::new(),
    );
    assert_eq!(refused(&formatted).kind, RefusalKind::LooksLikeDecision);
    let pluralized = screen(
        vec![text(
            "root",
            json!({
                "call": "pluralize",
                "args": { "value": 3, "one": "Take over", "other": "invoices" },
                "returnType": "string"
            }),
        )],
        json!({}),
        Vec::new(),
    );
    assert_eq!(
        refused(&pluralized).kind,
        RefusalKind::LooksLikeDecision,
        "every plural form is checked"
    );
}

#[test]
fn a_reserved_word_in_a_template_list_item_is_refused() {
    let draft = screen(
        vec![
            json!({ "id": "root", "component": "List", "children": { "componentId": "item", "path": "/items" } }),
            text("item", json!({ "path": "name" })),
        ],
        json!({ "items": [{ "name": "Invoice" }, { "name": "Stop all" }] }),
        Vec::new(),
    );
    assert_eq!(
        refused(&draft),
        refusal(
            RefusalKind::LooksLikeDecision,
            Some(("item", "Text")),
            "text matches reserved word \"Stop all\" after normalisation"
        )
    );
}

#[test]
fn stop_all_and_take_over_are_refused_as_labels_and_edit_address_is_not() {
    for label in ["Stop all", "Take over", "Edit", "Done", "Deny", "Approve"] {
        let mut draft = invoice_form();
        draft.actions[0].label = label.to_string();
        assert_eq!(refused(&draft).kind, RefusalKind::LooksLikeDecision, "{label}");
    }
    let mut draft = invoice_form();
    draft.components[2] = text("send-label", json!("Edit address"));
    draft.actions[0].label = "Edit address".into();
    assert!(Validator::forms().validate(&draft).is_ok());
}

#[test]
fn images_icons_and_unknown_components_are_refused() {
    for component in ["Image", "Icon", "Video", "AudioPlayer", "Modal", "Iframe"] {
        let draft = screen(
            vec![json!({ "id": "root", "component": component, "url": "x" })],
            json!({}),
            Vec::new(),
        );
        assert_eq!(
            refused(&draft),
            refusal(
                RefusalKind::CannotShow,
                Some(("root", component)),
                &format!("{component} is not in the catalog")
            ),
        );
    }
}

#[test]
fn a_secret_field_is_refused_with_take_over_in_the_note() {
    let obscured = screen(
        column(vec![
            json!({ "id": "pw", "component": "TextField", "label": "Code", "variant": "obscured" }),
        ]),
        json!({}),
        Vec::new(),
    );
    let refusal_found = refused(&obscured);
    assert_eq!(
        refusal_found,
        refusal(
            RefusalKind::AsksForSecret,
            Some(("pw", "TextField")),
            "an obscured field asks for a secret"
        )
    );
    assert!(
        refusal_found.message().contains("use Take over for passwords"),
        "{}",
        refusal_found.message()
    );
    for label in ["Password", "Your PIN", "API key", "Card number"] {
        let draft = screen(
            column(vec![json!({ "id": "f", "component": "TextField", "label": label })]),
            json!({}),
            Vec::new(),
        );
        assert_eq!(refused(&draft).kind, RefusalKind::AsksForSecret, "{label}");
    }
}

#[test]
fn functions_other_than_checks_and_text_are_refused() {
    let open_url = screen(
        column(vec![
            text("label", json!("Open")),
            json!({
                "id": "b", "component": "Button", "child": "label",
                "action": { "functionCall": { "call": "openUrl", "args": { "url": "x" } } }
            }),
        ]),
        json!({}),
        Vec::new(),
    );
    assert_eq!(
        refused(&open_url),
        refusal(
            RefusalKind::CannotShow,
            Some(("b", "Button")),
            "a button runs only a declared action"
        )
    );
    let date = screen(
        vec![text(
            "root",
            json!({ "call": "formatDate", "args": { "value": "2026-10-26" }, "returnType": "string" }),
        )],
        json!({}),
        Vec::new(),
    );
    assert_eq!(
        refused(&date),
        refusal(
            RefusalKind::CannotShow,
            Some(("root", "Text")),
            "calls the function 'formatDate', which a drawing may not use"
        )
    );
}

#[test]
fn a_link_anywhere_is_refused() {
    let in_text =
        screen(vec![text("root", json!("Pay at https://pay.example.com"))], json!({}), Vec::new());
    assert_eq!(refused(&in_text).kind, RefusalKind::CannotShow);
    let in_data = screen(
        vec![text("root", json!({ "path": "/t" }))],
        json!({ "t": "www.example.com" }),
        Vec::new(),
    );
    assert_eq!(refused(&in_data).kind, RefusalKind::CannotShow);
    let mut in_constant = invoice_form();
    in_constant.actions[0]
        .args
        .insert("link".into(), ActionArg::Constant(json!("javascript:alert(1)")));
    assert_eq!(refused(&in_constant).kind, RefusalKind::CannotShow);
}

#[test]
fn buttons_dispatch_only_declared_actions_bound_to_real_fields() {
    let mut undeclared = invoice_form();
    undeclared.components[3]["action"]["event"]["name"] = json!("approve");
    assert_eq!(
        refused(&undeclared),
        refusal(
            RefusalKind::CannotShow,
            Some(("send-button", "Button")),
            "the button's action 'approve' is not declared"
        )
    );
    let mut unbound = invoice_form();
    unbound.actions[0]
        .args
        .insert("amount".into(), ActionArg::Field(FieldArg { path: "/amount".into() }));
    assert_eq!(
        refused(&unbound),
        refusal(
            RefusalKind::CannotShow,
            None,
            "action 'send' binds 'amount' to /amount, which no field fills"
        )
    );
    for tool in ["kernel_spawn", "render_form"] {
        let mut draft = invoice_form();
        draft.actions[0].tool = tool.to_string();
        assert_eq!(refused(&draft).kind, RefusalKind::CannotShow, "{tool}");
    }
    let only_mine = Validator::forms().with_bindable_tools(["draft_invoice"]);
    assert_eq!(only_mine.validate(&invoice_form()).unwrap_err().kind, RefusalKind::CannotShow);
    assert!(
        Validator::forms().with_bindable_tools(["send_invoice"]).validate(&invoice_form()).is_ok()
    );
}

#[test]
fn a_card_has_no_buttons_or_fields() {
    let mut card = invoice_form();
    card.kind = DrawnKind::Card;
    card.actions.clear();
    assert_eq!(
        refused(&card),
        refusal(
            RefusalKind::CannotShow,
            Some(("send-button", "Button")),
            "a card or table has no buttons"
        )
    );
}

#[test]
fn malformed_trees_are_refused() {
    let no_root = screen(vec![text("title", json!("x"))], json!({}), Vec::new());
    assert_eq!(
        refused(&no_root),
        refusal(RefusalKind::CannotShow, None, "no component has the id 'root'")
    );
    let twice =
        screen(vec![text("root", json!("a")), text("root", json!("b"))], json!({}), Vec::new());
    assert_eq!(
        refused(&twice),
        refusal(RefusalKind::CannotShow, Some(("root", "Text")), "id 'root' is used twice")
    );
    let missing = screen(
        vec![json!({ "id": "root", "component": "Card", "child": "gone" })],
        json!({}),
        Vec::new(),
    );
    assert_eq!(
        refused(&missing),
        refusal(RefusalKind::CannotShow, Some(("root", "Card")), "child 'gone' does not exist")
    );
    let looped = screen(
        vec![
            json!({ "id": "root", "component": "Card", "child": "a" }),
            json!({ "id": "a", "component": "Card", "child": "root" }),
        ],
        json!({}),
        Vec::new(),
    );
    assert_eq!(
        refused(&looped),
        refusal(RefusalKind::CannotShow, Some(("root", "Card")), "the component tree loops")
    );
    let mut deep = vec![json!({ "id": "root", "component": "Card", "child": "c1" })];
    for level in 1..=20 {
        deep.push(json!({ "id": format!("c{level}"), "component": "Card", "child": format!("c{}", level + 1) }));
    }
    deep.push(text("c21", json!("bottom")));
    assert_eq!(refused(&screen(deep, json!({}), Vec::new())).kind, RefusalKind::CannotShow);
    let extra_property = screen(
        vec![json!({ "id": "root", "component": "Text", "text": "x", "html": "<b>" })],
        json!({}),
        Vec::new(),
    );
    assert_eq!(refused(&extra_property).kind, RefusalKind::CannotShow);
}

/// Text built from `word` by changing the case of letters, putting zero-width characters
/// between them and swapping some letters for Cyrillic look-alikes.
fn disguise(word: &'static str) -> impl Strategy<Value = String> {
    let len = word.chars().count();
    (
        proptest::collection::vec(any::<bool>(), len),
        proptest::collection::vec(
            prop_oneof![Just(""), Just("\u{200B}"), Just("\u{2060}"), Just("\u{00AD}")],
            len,
        ),
        proptest::collection::vec(any::<bool>(), len),
    )
        .prop_map(move |(upper, gaps, swap)| {
            word.chars()
                .enumerate()
                .map(|(index, c)| {
                    let c = match (swap[index], c) {
                        (true, 'a') => '\u{0430}',
                        (true, 'o') => '\u{043E}',
                        (true, 'e') => '\u{0435}',
                        (true, 'c') => '\u{0441}',
                        (true, 'p') => '\u{0440}',
                        (_, c) => c,
                    };
                    let c = if upper[index] {
                        c.to_uppercase().collect::<String>()
                    } else {
                        c.to_string()
                    };
                    format!("{c}{}", gaps[index])
                })
                .collect()
        })
}

fn any_reserved() -> impl Strategy<Value = &'static str> {
    proptest::sample::select(
        RESERVED_PHRASES.iter().chain(RESERVED_LABELS).copied().collect::<Vec<_>>(),
    )
}

fn arbitrary_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        "[a-zA-Z0-9 /:.${}]{0,12}".prop_map(Value::String),
        Just(json!("root")),
        Just(json!("Text")),
        Just(json!("Button")),
    ];
    leaf.prop_recursive(4, 40, 6, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            proptest::collection::btree_map(
                prop_oneof![
                    Just("id".to_string()),
                    Just("component".to_string()),
                    Just("text".to_string()),
                    Just("child".to_string()),
                    Just("children".to_string()),
                    Just("path".to_string()),
                    Just("call".to_string()),
                    "[a-z]{1,6}",
                ],
                inner,
                0..6
            )
            .prop_map(|map| Value::Object(map.into_iter().collect())),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// A reserved word, however it is written, never passes as a whole label.
    #[test]
    fn a_disguised_reserved_word_never_passes(word in any_reserved().prop_flat_map(disguise)) {
        let draft = screen(vec![text("root", json!(word.clone()))], json!({}), Vec::new());
        let refusal = Validator::forms().validate(&draft).unwrap_err();
        prop_assert_eq!(refusal.kind, RefusalKind::LooksLikeDecision, "{:?}", word);
    }

    /// A reserved phrase inside other words never passes.
    #[test]
    fn a_reserved_phrase_inside_a_sentence_never_passes(
        before in "[a-z]{1,8}",
        phrase in proptest::sample::select(RESERVED_PHRASES.to_vec()),
        after in "[a-z]{1,8}",
    ) {
        let draft = screen(vec![text("root", json!(format!("{before} {phrase} {after}")))], json!({}), Vec::new());
        prop_assert_eq!(Validator::forms().validate(&draft).unwrap_err().kind, RefusalKind::LooksLikeDecision);
    }

    /// A component type outside the catalog never passes.
    #[test]
    fn an_off_catalog_component_never_passes(name in "[A-Z][a-zA-Z]{2,12}") {
        prop_assume!(!Validator::forms().catalog().has_component(&name));
        let draft = screen(vec![json!({ "id": "root", "component": name })], json!({}), Vec::new());
        prop_assert_eq!(Validator::forms().validate(&draft).unwrap_err().kind, RefusalKind::CannotShow);
    }

    /// Validation is idempotent: what passed passes again, with the same strings.
    #[test]
    fn validation_is_idempotent(name in "[A-Za-z ]{1,20}", status in "[A-Za-z0-9 ]{0,20}") {
        let draft = screen(
            column(vec![text("name", json!(name)), text("status", json!({ "path": "/status" }))]),
            json!({ "status": status }),
            Vec::new(),
        );
        let validator = Validator::forms();
        match validator.validate(&draft) {
            Ok(first) => prop_assert_eq!(validator.validate(&first.draft), Ok(first)),
            Err(refusal) => prop_assert_eq!(validator.validate(&draft), Err(refusal)),
        }
    }

    /// Validating arbitrary JSON never panics.
    #[test]
    fn arbitrary_json_never_panics(components in proptest::collection::vec(arbitrary_json(), 0..8), model in arbitrary_json()) {
        let _ = Validator::forms().validate(&screen(components, model, Vec::new()));
    }

    /// JSON lines round-trip.
    #[test]
    fn jsonl_round_trips(ids in proptest::collection::vec("[a-z0-9-]{1,12}", 1..6), value in arbitrary_json()) {
        let messages: Vec<A2uiMessage> = ids
            .iter()
            .flat_map(|id| {
                [
                    A2uiMessage::create_surface(id.clone()),
                    A2uiMessage::update_components(id.clone(), vec![text("root", json!("x"))]),
                    A2uiMessage::update_data_model(id.clone(), Some("/v".into()), Some(value.clone()).filter(|v| !v.is_null())),
                    A2uiMessage::delete_surface(id.clone()),
                ]
            })
            .collect();
        prop_assert_eq!(decode_jsonl(&encode_jsonl(&messages)).unwrap(), messages);
    }
}
