use super::*;
use serde_json::json;

fn origin() -> ProviderOrigin {
    ProviderOrigin {
        provider: "responses".into(),
        model: "reasoning-model".into(),
        scope: "owner".into(),
    }
}

fn call(signature: Option<&str>) -> Part {
    Part::FunctionCall {
        name: "lookup".into(),
        args: json!({"key":"value"}),
        id: Some("call_1".into()),
        thought_signature: signature.map(str::to_owned),
    }
}

fn content() -> Content {
    Content {
        role: "model".into(),
        parts: vec![
            Part::Thinking {
                thinking: "Private thought".into(),
                signature: Some("private-signature".into()),
            },
            Part::Text { text: "Visible answer".into() },
            call(Some("call-signature")),
        ],
    }
}

fn metadata() -> serde_json::Value {
    json!({"provider_origin": origin(), "openai":{"conversation_history_parts":[{
        "server_tool_call":{"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"opaque"}
    }]}})
}

#[test]
fn preserves_source_order_and_deduplicates() {
    let original = content();
    let metadata = metadata();
    let bytes = serde_json::to_vec(&original).unwrap();
    let history = with_provider_parts(&original, Some(&metadata));
    assert!(is_provider_replay(&history.parts[0]));
    assert_eq!(replay_part(&history.parts[0]).as_ref(), Some(&original.parts[0]));
    assert_eq!(history.parts[1], original.parts[1]);
    assert_eq!(history.parts[2], call(None));
    assert!(history.has_function_calls());
    let twice = with_provider_parts(&history, Some(&metadata));
    assert_eq!(twice.parts, history.parts);
    assert_eq!(serde_json::to_vec(&original).unwrap(), bytes);
    let mut sent = vec![history];
    for_provider(&mut sent, &origin());
    assert_eq!(&sent[0].parts[..3], &original.parts);
    assert!(matches!(sent[0].parts[3], Part::ServerToolCall { .. }));
    assert!(!serde_json::to_string(&sent).unwrap().contains("__adk_provider_replay"));
}

#[test]
fn isolates_each_ownership_component() {
    for field in ["provider", "model", "scope"] {
        let mut target = origin();
        match field {
            "provider" => target.provider = "gemini".into(),
            "model" => target.model = "other-model".into(),
            "scope" => target.scope = "other-account-or-endpoint".into(),
            _ => unreachable!(),
        }
        let projected = with_provider_parts(&content(), Some(&metadata()));
        let bytes = serde_json::to_vec(&projected).unwrap();
        let mut sent = vec![projected.clone()];
        for_provider(&mut sent, &target);
        assert_eq!(
            sent[0].parts,
            vec![Part::Text { text: "Visible answer".into() }, call(None)],
            "{field}"
        );
        assert_eq!(serde_json::to_vec(&projected).unwrap(), bytes);
    }
}

#[test]
fn treats_old_records_as_unowned() {
    let old = with_provider_parts(&content(), Some(&json!({"openai": metadata()["openai"]})));
    let mut sent = vec![old.clone(), Content::new("model").with_thinking("Old thought")];
    for_provider(&mut sent, &origin());
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].parts, vec![Part::Text { text: "Visible answer".into() }, call(None)]);
    assert!(!old.parts.iter().any(is_provider_replay));
}

#[test]
fn keeps_results_and_rejects_invalid_records() {
    let response = Part::FunctionResponse {
        function_response: crate::FunctionResponseData::new("lookup", json!({"value":42})),
        id: Some("call_1".into()),
        annotations: None,
    };
    let nested = encode(
        &origin(),
        ReplayData::Opaque {
            part: encode(&origin(), ReplayData::Opaque { part: content().parts[0].clone() }),
        },
    );
    let malformed = Part::ServerToolCall {
        server_tool_call: json!({REPLAY_KEY: {"origin": origin(), "kind": "unknown"}}),
    };
    assert!(nested.for_model().is_none());
    assert!(malformed.for_model().is_none());
    let mut sent =
        vec![Content { role: "tool".into(), parts: vec![nested, malformed, response.clone()] }];
    for_provider(&mut sent, &origin());
    assert_eq!(sent[0].parts, vec![response]);
}

#[test]
fn preserves_generated_call_ids() {
    let mut source = content();
    if let Part::FunctionCall { id, .. } = &mut source.parts[2] {
        *id = None;
    }
    let mut projected = with_provider_parts(&source, Some(&metadata()));
    projected.assign_call_ids();
    assert!(matches!(projected.parts[2], Part::FunctionCall { id: Some(_), .. }));
    projected.strip_generated_call_ids();
    assert!(matches!(projected.parts[2], Part::FunctionCall { id: None, .. }));
}

#[test]
fn owned_data_requires_projection() {
    let projected = with_provider_parts(&content(), Some(&metadata()));
    let model = projected.for_model();
    assert_eq!(model.parts, vec![Part::Text { text: "Visible answer".into() }, call(None)]);
    assert_eq!(replay_part(&projected.parts[0]).as_ref(), Some(&content().parts[0]));
}

#[test]
fn originless_parts_replay_unchanged() {
    let replay = Part::ServerToolCall {
        server_tool_call: json!({"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"opaque"}),
    };
    for metadata in [
        None,
        Some(json!({"openai": metadata()["openai"]})),
        Some(json!({"provider_origin":{"provider":"","model":"m","scope":"s"},
            "openai": metadata()["openai"]})),
    ] {
        let projected = with_provider_parts(&content(), metadata.as_ref());
        let mut expected = content().parts;
        if metadata.is_some() {
            expected.push(replay.clone());
        }
        assert_eq!(projected.parts, expected);
        assert_eq!(projected.for_model().parts, expected);
    }
}

#[test]
fn roundtrips_owned_history() {
    let projected = with_provider_parts(&content(), Some(&metadata()));
    let encoded = serde_json::to_value(&projected).unwrap();
    assert_eq!(encoded["parts"][2]["name"], "lookup");
    assert!(encoded["parts"][2].get("thought_signature").is_none());
    assert_eq!(encoded["parts"][3]["server_tool_call"][REPLAY_KEY]["origin"], json!(origin()));
    let restored: Content = serde_json::from_value(encoded).unwrap();
    assert_eq!(restored.role, projected.role);
    assert_eq!(restored.parts, projected.parts);
    assert!(restored.has_function_calls());
    assert_eq!(restored.for_model().parts, projected.for_model().parts);
    let mut sent = vec![restored];
    for_provider(&mut sent, &origin());
    assert_eq!(&sent[0].parts[..3], &content().parts);
}

#[test]
fn refuses_changed_or_nonadjacent_calls() {
    for mutation in ["name", "args", "id", "separated", "missing"] {
        let mut projected = with_provider_parts(&content(), Some(&metadata()));
        if let Part::FunctionCall { name, args, id, .. } = &mut projected.parts[2] {
            match mutation {
                "name" => *name = "other".into(),
                "args" => *args = json!({"changed":true}),
                "id" => *id = Some("other-call".into()),
                _ => {}
            }
        }
        if mutation == "separated" {
            projected.parts.insert(3, Part::Text { text: "Between".into() });
        }
        if mutation == "missing" {
            projected.parts.remove(2);
        }
        let mut sent = vec![projected];
        for_provider(&mut sent, &origin());
        assert!(!serde_json::to_string(&sent).unwrap().contains("call-signature"), "{mutation}");
        assert_eq!(
            sent[0].parts.iter().filter(|p| matches!(p, Part::FunctionCall { .. })).count(),
            usize::from(mutation != "missing")
        );
    }
}

#[test]
fn duplicate_calls_keep_distinct_signatures() {
    let source =
        Content { role: "model".into(), parts: vec![call(Some("first")), call(Some("second"))] };
    let projected = with_provider_parts(&source, Some(&metadata()));
    assert_eq!(
        projected.parts.iter().filter(|p| matches!(p, Part::FunctionCall { .. })).count(),
        2
    );
    let mut sent = vec![projected];
    for_provider(&mut sent, &origin());
    assert_eq!(&sent[0].parts[..2], &source.parts);
}

#[test]
fn keeps_records_out_of_context_and_summaries() {
    let projected = with_provider_parts(&content(), Some(&metadata()));
    let context = crate::history::other_agent_context("other", &projected).unwrap();
    let rendered = serde_json::to_string(&context).unwrap();
    for forbidden in [REPLAY_KEY, "signature", "Private thought", "opaque"] {
        assert!(!rendered.contains(forbidden), "{forbidden}: {rendered}");
    }
    assert!(rendered.contains("lookup") && rendered.contains("Visible answer"));
    for part in projected.parts.iter().filter(|p| is_provider_replay(p)) {
        assert!(crate::intra_compaction::render_part_for_summary(part).is_none());
    }
}

#[test]
fn reentry_keeps_call_pairs_after_id_assignment() {
    let mut source = Content { role: "model".into(), parts: vec![call(Some("signed"))] };
    if let Part::FunctionCall { id, .. } = &mut source.parts[0] {
        *id = None;
    }
    let metadata = json!({"provider_origin":origin(), "conversation_history_parts":source.parts});
    let mut projected = with_provider_parts(&source, Some(&metadata));
    projected.assign_call_ids();
    let reentered = with_provider_parts(&projected, Some(&metadata));
    assert_eq!(reentered.parts, projected.parts);
    let mut sent = vec![reentered];
    for_provider(&mut sent, &origin());
    assert!(
        matches!(&sent[0].parts[0], Part::FunctionCall { id: Some(id), thought_signature: Some(signature), .. }
        if id.starts_with(crate::GENERATED_CALL_ID_PREFIX) && signature == "signed")
    );
}
