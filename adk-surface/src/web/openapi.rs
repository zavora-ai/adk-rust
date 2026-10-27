//! The OpenAPI document of the web API, generated from the Rust types, and the TypeScript
//! types generated from it (spec 3.3 §6). `scripts/check-surface-api.sh` writes both under
//! `adk-surface/web/src/generated/` and CI fails when they drift from the Rust source.
//!
//! **One document in every build.** Every surface's wire types and paths — the phone's, voice's,
//! the tray's — compile under `web`, never under that surface's own feature (`mobile`,
//! `voice`), so this document is the same in every `web` build, including adk-cli's default
//! one. `tests/api_contract.rs` and `scripts/check-surface-api.sh` check it with
//! `--features web` alone.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use schemars::JsonSchema;
use schemars::generate::{Contract, SchemaGenerator, SchemaSettings};
use serde_json::{Map, Value, json};

use super::phone::{
    AgentConfirmOptionsRequest, AnswerRequest, ConfirmOptions, ConfirmOptionsRequest,
    ConfirmRequest, DeliveriesResponse, PasskeyOptions, PasskeyRegistration, PhoneDevicesResponse,
    PhoneInfo, PushReceivedRequest, PushSubscriptionRequest, QrRequest, StopRequest,
};
use super::{
    AgentsResponse, CommandRequest, DecisionsResponse, QueuedResponse, SurfaceQuery,
    TakeOverRequest, ThreadResponse, paths,
};
use crate::engine::{CommandOutcome, FeedFrame};
use crate::model::{Briefing, DecisionCard, FeedEntry, StatusLine};
use crate::voice::{
    VoiceClientMessage, VoiceModels, VoiceTicket, VoiceTicketRequest, VoiceUpdate,
    paths as voice_paths,
};

// The website's document (spec 3.5 §8.2): its own, so this one is the same in every `web` build.
#[cfg(feature = "website")]
pub(super) mod website;
// The screen actions' document (spec 3.3 §2 item 7): its own, for the same reason.
#[cfg(feature = "verb-ui")]
pub(super) mod verbs;

fn generator(contract: Contract) -> SchemaGenerator {
    SchemaSettings::draft2020_12()
        .with(|settings| {
            settings.definitions_path = "/components/schemas".into();
            settings.meta_schema = None;
            settings.contract = contract;
        })
        .into_generator()
}

fn schema_ref<T: JsonSchema>(generator: &mut SchemaGenerator) -> Value {
    generator.subschema_for::<T>().to_value()
}

fn answer(description: &str, schema: Value) -> Value {
    json!({
        "description": description,
        "content": { "application/json": { "schema": schema } },
    })
}

fn problem() -> Value {
    json!({
        "description": "A problem document with the engine's `surface.*` code, or the source's",
        "content": { "application/problem+json": { "schema": { "type": "object" } } },
    })
}

fn errors(mut responses: Map<String, Value>) -> Value {
    for status in ["401", "403", "404"] {
        responses.insert(status.to_string(), problem());
    }
    Value::Object(responses)
}

fn read(summary: &str, parameters: Value, ok: Value) -> Value {
    let mut responses = Map::new();
    responses.insert("200".into(), ok);
    json!({ "get": { "summary": summary, "parameters": parameters, "responses": errors(responses) } })
}

fn query(name: &str, description: &str, schema: Value) -> Value {
    json!({ "name": name, "in": "query", "required": false, "description": description, "schema": schema })
}

/// A `POST` (or `PUT`, `DELETE`) route with a JSON body when `body` is set.
fn write(
    method: &str,
    summary: &str,
    parameters: Value,
    body: Option<Value>,
    ok: (&str, Value),
) -> Value {
    let mut operation = Map::new();
    operation.insert("summary".into(), json!(summary));
    operation.insert("parameters".into(), parameters);
    if let Some(body) = body {
        operation.insert(
            "requestBody".into(),
            json!({ "required": true, "content": { "application/json": { "schema": body } } }),
        );
    }
    let mut responses = Map::new();
    responses.insert(ok.0.into(), ok.1);
    responses.insert("400".into(), problem());
    operation.insert("responses".into(), errors(responses));
    let mut route = Map::new();
    route.insert(method.into(), Value::Object(operation));
    Value::Object(route)
}

fn no_content(description: &str) -> (&'static str, Value) {
    ("204", json!({ "description": description }))
}

fn path_param(name: &str) -> Value {
    json!({ "name": name, "in": "path", "required": true, "schema": { "type": "string" } })
}

/// The OpenAPI 3.1 document of [`router`](super::router), its schemas generated from the
/// Rust types (spec 3.3 §6). Paths are under `/api`.
///
/// # Example
///
/// ```rust
/// let document = adk_surface::web::openapi();
/// assert_eq!(document["openapi"], "3.1.0");
/// assert!(document["components"]["schemas"]["DecisionCard"].is_object());
/// ```
pub fn openapi() -> Value {
    let mut out = generator(Contract::Serialize);
    let decisions = schema_ref::<DecisionsResponse>(&mut out);
    let card = schema_ref::<DecisionCard>(&mut out);
    let thread = schema_ref::<ThreadResponse>(&mut out);
    let agents = schema_ref::<AgentsResponse>(&mut out);
    let status = schema_ref::<StatusLine>(&mut out);
    let briefing = schema_ref::<Briefing>(&mut out);
    let outcome = schema_ref::<CommandOutcome>(&mut out);
    let frame = schema_ref::<FeedFrame>(&mut out);
    let entry = schema_ref::<FeedEntry>(&mut out);
    let queued = schema_ref::<QueuedResponse>(&mut out);
    let phone = schema_ref::<PhoneInfo>(&mut out);
    let deliveries = schema_ref::<DeliveriesResponse>(&mut out);
    let passkey_options = schema_ref::<PasskeyOptions>(&mut out);
    let confirm_options = schema_ref::<ConfirmOptions>(&mut out);
    let phone_devices = schema_ref::<PhoneDevicesResponse>(&mut out);
    let voice_models = schema_ref::<VoiceModels>(&mut out);
    let voice_ticket = schema_ref::<VoiceTicket>(&mut out);
    let voice_update = schema_ref::<VoiceUpdate>(&mut out);
    let agent_detail = schema_ref::<crate::agents::AgentDetail>(&mut out);
    let agent_choices = schema_ref::<crate::agents::AgentChoices>(&mut out);
    let agent_preview = schema_ref::<crate::agents::AgentPreview>(&mut out);
    let mut schemas = out.take_definitions(true);

    let mut input = generator(Contract::Deserialize);
    let request = schema_ref::<CommandRequest>(&mut input);
    let take_over = schema_ref::<TakeOverRequest>(&mut input);
    let answer_body = schema_ref::<AnswerRequest>(&mut input);
    let stop_body = schema_ref::<StopRequest>(&mut input);
    let confirm_options_body = schema_ref::<ConfirmOptionsRequest>(&mut input);
    let confirm_body = schema_ref::<ConfirmRequest>(&mut input);
    let agent_confirm_options_body = schema_ref::<AgentConfirmOptionsRequest>(&mut input);
    let push_body = schema_ref::<PushSubscriptionRequest>(&mut input);
    let received_body = schema_ref::<PushReceivedRequest>(&mut input);
    let registration_body = schema_ref::<PasskeyRegistration>(&mut input);
    let qr_body = schema_ref::<QrRequest>(&mut input);
    let voice_request = schema_ref::<VoiceTicketRequest>(&mut input);
    let voice_message = schema_ref::<VoiceClientMessage>(&mut input);
    let preview_body = schema_ref::<crate::agents::AgentPreviewRequest>(&mut input);
    let surface_query = input.subschema_for::<SurfaceQuery>().to_value();
    for (name, schema) in input.take_definitions(true) {
        schemas.entry(name).or_insert(schema);
    }
    let surface = surface_query["properties"]["surface"].clone();

    let mut routes = BTreeMap::new();
    let with = |path: &str| format!("/api{path}");
    routes.insert(
        with(paths::DECISIONS),
        read(
            "The caller's open decisions as cards (spec 3.3 §7.1)",
            json!([
                query("surface", "The surface the answers are computed for", surface.clone()),
                query("session", "Only this session's decisions", json!({ "type": "string" })),
            ]),
            answer("Open decisions, oldest first", decisions),
        ),
    );
    routes.insert(
        with(paths::DECISION),
        read(
            "One decision: waiting, or decided with who, where and when",
            json!([
                path_param("id"),
                query("surface", "The surface the answers are computed for", surface.clone())
            ]),
            answer("The decision", card),
        ),
    );
    routes.insert(
        with(paths::THREAD),
        read(
            "The session's entries, numbered by their position in the session store",
            json!([path_param("session")]),
            answer("The thread", thread),
        ),
    );
    routes.insert(
        with(paths::FEED),
        json!({ "get": {
            "summary": "The session's feed as server-sent events: `entry` frames carry `id` = `seq` and resume after `Last-Event-ID`; `decisions` and `reset` frames carry no id",
            "parameters": [
                path_param("session"),
                query("after", "Resume after this seq when no Last-Event-ID header is sent", json!({ "type": "integer", "minimum": 0 })),
                { "name": "Last-Event-ID", "in": "header", "required": false, "schema": { "type": "string" } },
            ],
            "responses": errors({
                let mut ok = Map::new();
                ok.insert("200".into(), json!({
                    "description": "`text/event-stream` of FeedFrame payloads; an `entry` event's data is a FeedEntry",
                    "content": { "text/event-stream": { "schema": frame, "x-entry": entry } },
                }));
                ok
            }),
        } }),
    );
    routes.insert(
        with(paths::QUEUED),
        read(
            "What the caller typed while a decision of the session was open, sent once it is answered (spec 3.3 §7.4 rule 2)",
            json!([path_param("session")]),
            answer("The queued input, oldest first", queued),
        ),
    );
    routes.insert(
        with(paths::AGENTS),
        read("The caller's agents (spec 3.3 §8.1)", json!([]), answer("The roster", agents)),
    );
    // Agents a person adds (spec 3.3 §5.7); every change goes through `POST /surface/commands`.
    routes.insert(
        with(paths::AGENT),
        read(
            "One agent the caller added: its definition, version and how it was added",
            json!([path_param("id")]),
            answer("The agent", agent_detail),
        ),
    );
    routes.insert(
        with(paths::AGENT_CHOICES),
        read(
            "What the caller may choose for an agent: the models whose key the kernel has, the packs, the areas and the colours",
            json!([]),
            answer("The choices", agent_choices),
        ),
    );
    routes.insert(
        with(paths::AGENT_PREVIEW),
        write(
            "post",
            "Here is what Finance can do: the abilities, the risk and the digest a change applies with; closed to paired devices",
            json!([]),
            Some(preview_body),
            ("200", answer("The preview", agent_preview)),
        ),
    );
    routes.insert(
        with(paths::STATUS),
        read(
            "The status line: mode, spend against the daily limit, quiet time, decisions",
            json!([query(
                "session",
                "The session whose mode to show",
                json!({ "type": "string" })
            )]),
            answer("The status line", status),
        ),
    );
    routes.insert(
        with(paths::BRIEFING),
        read(
            "While you were away (spec 3.3 §5.1)",
            json!([query(
                "since",
                "Since when; default the last 24 hours",
                json!({ "type": "string", "format": "date-time" })
            )]),
            answer("The briefing", briefing),
        ),
    );
    routes.insert(
        with(paths::COMMANDS),
        json!({ "post": {
            "summary": "A command, with the surface and how it was given (spec 3.3 §6, §7.2, §7.4, §11)",
            "requestBody": { "required": true, "content": { "application/json": { "schema": request } } },
            "responses": errors({
                let mut ok = Map::new();
                ok.insert("200".into(), answer("What the command did", outcome.clone()));
                ok.insert("422".into(), problem());
                ok
            }),
        } }),
    );
    routes.insert(
        with(paths::TAKE_OVER),
        json!({ "post": {
            "summary": "Take over, then Done (spec 3.3 §8.2, §10 H2): the secret the person typed goes to the one call that asked; mounted beside a harness that takes over",
            "parameters": [path_param("id")],
            "requestBody": { "required": true, "content": { "application/json": { "schema": take_over } } },
            "responses": errors({
                let mut ok = Map::new();
                ok.insert("200".into(), answer("answered, alreadyDecided or changed", outcome.clone()));
                ok.insert("422".into(), problem());
                ok
            }),
        } }),
    );
    // Paired devices and the phone (spec 3.3 §5.4, §8.2, §10 K2).
    let outcome_of = |description: &str| ("200", answer(description, outcome.clone()));
    routes.insert(
        with(paths::ANSWER),
        write(
            "post",
            "A paired device's answer at the version it saw, as its kind's surface (spec 3.3 §7.2, §10 K2)",
            json!([path_param("id")]),
            Some(answer_body),
            outcome_of("answered, alreadyDecided or changed"),
        ),
    );
    routes.insert(
        with(paths::STOP),
        write(
            "post",
            "Stop all from a paired device (spec 3.3 §11 S-I10); every other caller sends a stopAll command",
            json!([]),
            Some(stop_body),
            outcome_of("stopped"),
        ),
    );
    routes.insert(
        with(paths::CONFIRM_OPTIONS),
        write(
            "post",
            "Starts Confirm it's you for one answer, bound to the decision, its version and the edited arguments; feature mobile (spec 3.3 §8.2)",
            json!([path_param("id")]),
            Some(confirm_options_body),
            ("200", answer("The options for navigator.credentials.get", confirm_options.clone())),
        ),
    );
    routes.insert(
        with(paths::CONFIRM),
        write(
            "post",
            "The passkey's assertion for the bound answer, which is then decided with presence; feature mobile (spec 3.3 §8.2)",
            json!([path_param("id")]),
            Some(confirm_body.clone()),
            outcome_of("answered, alreadyDecided or changed"),
        ),
    );
    routes.insert(
        with(paths::AGENT_CONFIRM_OPTIONS),
        write(
            "post",
            "Starts Confirm it's you for one change that adds an agent or lets one do more at high risk, bound to exactly that command; feature mobile (spec 3.3 §7.2)",
            json!([]),
            Some(agent_confirm_options_body),
            ("200", answer("The options for navigator.credentials.get", confirm_options.clone())),
        ),
    );
    routes.insert(
        with(paths::AGENT_CONFIRM),
        write(
            "post",
            "The passkey's assertion for the bound agent change, which is then made with presence; feature mobile (spec 3.3 §7.2)",
            json!([]),
            Some(confirm_body.clone()),
            outcome_of("agentAdded, agentChanged or agentOutOfDate"),
        ),
    );
    routes.insert(
        with(paths::PHONE),
        read(
            "What the phone needs to subscribe and to confirm it is you; feature mobile (spec 3.3 §5.4)",
            json!([]),
            answer("The phone's settings", phone),
        ),
    );
    let mut push = write(
        "put",
        "The phone's push subscription: https, on a browser's push service; feature mobile (spec 3.3 §5.4)",
        json!([]),
        Some(push_body),
        no_content("Subscribed"),
    );
    push["delete"] = write(
        "delete",
        "Stops the phone's wake-ups",
        json!([]),
        None,
        no_content("Removed"),
    )["delete"]
        .take();
    routes.insert(with(paths::PHONE_PUSH), push);
    routes.insert(
        with(paths::PHONE_PUSH_RECEIVED),
        write(
            "post",
            "The phone received a wake-up; its latency is recorded",
            json!([]),
            Some(received_body),
            no_content("Recorded"),
        ),
    );
    routes.insert(
        with(paths::PHONE_PUSH_DELIVERIES),
        read(
            "The latest acknowledged wake-ups, for the operator",
            json!([]),
            answer("Deliveries, oldest first", deliveries),
        ),
    );
    routes.insert(
        with(paths::PHONE_PASSKEY_OPTIONS),
        write(
            "post",
            "Starts setting up a passkey: a phone within 10 minutes of pairing, or the person at the web home (spec 3.3 §8.2)",
            json!([]),
            None,
            ("200", answer("The options for navigator.credentials.create", passkey_options)),
        ),
    );
    routes.insert(
        with(paths::PHONE_PASSKEY),
        write(
            "post",
            "Finishes setting up a passkey",
            json!([]),
            Some(registration_body),
            no_content("Set up"),
        ),
    );
    routes.insert(
        with(paths::PHONE_PASSKEY_ONE),
        write(
            "delete",
            "Removes one of the caller's computer passkeys",
            json!([path_param("id")]),
            None,
            no_content("Removed"),
        ),
    );
    routes.insert(
        with(paths::PHONE_QR),
        write(
            "post",
            "A pairing link into this home as a QR code, for the operator",
            json!([]),
            Some(qr_body),
            (
                "200",
                json!({ "description": "The QR code", "content": { "image/svg+xml": { "schema": { "type": "string" } } } }),
            ),
        ),
    );
    routes.insert(
        with(paths::PHONE_DEVICES),
        read(
            "Each paired device's passkey and push, and the caller's computer passkeys, for the operator",
            json!([]),
            answer("The devices", phone_devices),
        ),
    );
    // Voice (spec 3.3 §5.6): its types are in every build's document; the routes are served
    // with the `voice` feature.
    routes.insert(
        with(voice_paths::VOICE),
        read(
            "The agent and the models voice can talk on, the default first (spec 3.3 §5.6); served with the `voice` feature",
            json!([]),
            answer("The voice models", voice_models),
        ),
    );
    routes.insert(
        with(voice_paths::SESSIONS),
        json!({ "post": {
            "summary": "A single-use ticket, good for 30 seconds, for the voice socket `GET /voice/socket` (spec 3.3 §5.6); served with the `voice` feature. The socket's first text message is `{\"type\":\"hello\",\"ticket\":…}`; then VoiceClientMessage up and VoiceUpdate down, and binary frames tagged 0x01 (PCM16 audio), 0x02 (camera JPEG) and 0x03 (screen JPEG)",
            "requestBody": { "required": true, "content": { "application/json": { "schema": voice_request } } },
            "responses": errors({
                let mut ok = Map::new();
                ok.insert("200".into(), json!({
                    "description": "The ticket",
                    "content": { "application/json": { "schema": voice_ticket } },
                    "x-socket": { "up": voice_message, "down": voice_update },
                }));
                ok
            }),
        } }),
    );
    routes.insert(
        with(paths::OPENAPI),
        json!({ "get": { "summary": "This document", "responses": { "200": { "description": "The OpenAPI document" } } } }),
    );

    let mut schemas = Value::Object(schemas);
    clean_descriptions(&mut schemas);
    sorted(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "adk-surface",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "The engine every agent surface runs on (spec 3.3 §6): decisions, threads, agents, the status line, the briefing and commands. Generated from the Rust types; do not edit.",
        },
        "paths": routes,
        "components": { "schemas": schemas },
    }))
}

/// `value` with every object's keys in sorted order, as `serde_json`'s default map keeps
/// them: the same document, and the same TypeScript, whether or not a crate in the build
/// turns on `serde_json/preserve_order`, which keeps keys in insertion order.
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            Value::Object(entries.into_iter().map(|(key, child)| (key, sorted(child))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// Keeps each description's prose: no rustdoc sections, no intra-doc link targets.
fn clean_descriptions(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                match (key.as_str(), child) {
                    ("description", Value::String(text)) => *text = prose(text),
                    (_, child) => clean_descriptions(child),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(clean_descriptions),
        _ => {}
    }
}

/// `text` up to its first rustdoc heading, with `[`X`](path)` and `[`X`]` as `` `X` ``.
fn prose(text: &str) -> String {
    let body = text.split("\n# ").next().unwrap_or(text).trim_end();
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '[' && chars.peek() == Some(&'`') {
            let mut inner = String::new();
            for next in chars.by_ref() {
                if next == ']' {
                    break;
                }
                inner.push(next);
            }
            if chars.peek() == Some(&'(') {
                for next in chars.by_ref() {
                    if next == ')' {
                        break;
                    }
                }
            }
            out.push_str(&inner);
        } else {
            out.push(c);
        }
    }
    out
}

/// TypeScript declarations of every schema in [`openapi`], for the web app (spec 3.3 §6).
///
/// # Example
///
/// ```rust
/// let ts = adk_surface::web::typescript();
/// assert!(ts.contains("export interface DecisionCard {"));
/// ```
pub fn typescript() -> String {
    let document = openapi();
    let mut out = String::from(
        "// Generated from the Rust types of adk-surface by `scripts/check-surface-api.sh --write`.\n\
         // Do not edit: CI fails when this file drifts from the Rust source (spec 3.3 §6).\n",
    );
    let Some(schemas) = document["components"]["schemas"].as_object() else {
        return out;
    };
    for (name, schema) in schemas {
        out.push('\n');
        if let Some(description) = schema.get("description").and_then(Value::as_str) {
            doc_comment(&mut out, description, "");
        }
        match object_body(schema, "") {
            Some(body) => {
                let _ = writeln!(out, "export interface {name} {body}");
            }
            None => {
                let _ = writeln!(out, "export type {name} = {};", ts_type(schema, ""));
            }
        }
    }
    out
}

fn doc_comment(out: &mut String, description: &str, indent: &str) {
    let lines: Vec<&str> = description.lines().collect();
    if lines.len() == 1 {
        let _ = writeln!(out, "{indent}/** {} */", lines[0].replace("*/", "*\\/"));
        return;
    }
    let _ = writeln!(out, "{indent}/**");
    for line in lines {
        let _ = writeln!(out, "{indent} * {}", line.replace("*/", "*\\/"));
    }
    let _ = writeln!(out, "{indent} */");
}

/// `{ a: T; b?: U }` for an object schema with properties.
fn object_body(schema: &Value, indent: &str) -> Option<String> {
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return None;
    }
    let properties = schema.get("properties")?.as_object()?;
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|required| required.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let inner = format!("{indent}  ");
    let mut body = String::from("{\n");
    for (key, property) in properties {
        if let Some(description) = property.get("description").and_then(Value::as_str) {
            doc_comment(&mut body, description, &inner);
        }
        let optional = if required.contains(&key.as_str()) { "" } else { "?" };
        let _ = writeln!(body, "{inner}{key}{optional}: {};", ts_type(property, &inner));
    }
    let _ = write!(body, "{indent}}}");
    Some(body)
}

fn ts_type(schema: &Value, indent: &str) -> String {
    match schema {
        Value::Bool(true) => return "unknown".to_string(),
        Value::Bool(false) => return "never".to_string(),
        _ => {}
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        return reference.rsplit('/').next().unwrap_or(reference).to_string();
    }
    if let Some(constant) = schema.get("const") {
        return constant.to_string();
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values.iter().map(Value::to_string).collect::<Vec<_>>().join(" | ");
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(variants) = schema.get(key).and_then(Value::as_array) {
            let mut union: Vec<String> = Vec::new();
            for variant in variants {
                let merged = merge_siblings(schema, variant);
                let text = ts_type(&merged, indent);
                if !union.contains(&text) {
                    union.push(text);
                }
            }
            return union.join(" | ");
        }
    }
    if let Some(all) = schema.get("allOf").and_then(Value::as_array) {
        return all.iter().map(|part| ts_type(part, indent)).collect::<Vec<_>>().join(" & ");
    }
    match schema.get("type") {
        Some(Value::Array(types)) => {
            let variants: Vec<String> = types
                .iter()
                .map(|kind| {
                    let mut single = schema.clone();
                    if let Some(object) = single.as_object_mut() {
                        object.insert("type".into(), kind.clone());
                    }
                    ts_type(&single, indent)
                })
                .collect();
            variants.join(" | ")
        }
        Some(Value::String(kind)) => match kind.as_str() {
            "string" => "string".to_string(),
            "integer" | "number" => "number".to_string(),
            "boolean" => "boolean".to_string(),
            "null" => "null".to_string(),
            "array" => {
                let item = schema
                    .get("items")
                    .map_or("unknown".to_string(), |items| ts_type(items, indent));
                if item.contains(' ') { format!("({item})[]") } else { format!("{item}[]") }
            }
            "object" => match object_body(schema, indent) {
                Some(body) => body,
                None => match schema.get("additionalProperties") {
                    Some(values) if values.is_object() => {
                        format!("Record<string, {}>", ts_type(values, indent))
                    }
                    _ => "Record<string, unknown>".to_string(),
                },
            },
            _ => "unknown".to_string(),
        },
        _ => "unknown".to_string(),
    }
}

/// A `oneOf` variant with the parent's object keywords it inherits (an internally tagged
/// enum's shared `type: object`).
fn merge_siblings(parent: &Value, variant: &Value) -> Value {
    let (Some(parent), Some(variant_map)) = (parent.as_object(), variant.as_object()) else {
        return variant.clone();
    };
    let mut merged = variant_map.clone();
    if let Some(kind) = parent.get("type")
        && !merged.contains_key("type")
        && !merged.contains_key("$ref")
    {
        merged.insert("type".into(), kind.clone());
    }
    Value::Object(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every object of `value` lists its keys in sorted order.
    fn keys_sorted(value: &Value) -> bool {
        match value {
            Value::Object(map) => {
                map.keys().zip(map.keys().skip(1)).all(|(a, b)| a <= b)
                    && map.values().all(keys_sorted)
            }
            Value::Array(items) => items.iter().all(keys_sorted),
            _ => true,
        }
    }

    /// Keys inserted in any order give the same document: with `serde_json/preserve_order`
    /// on, a map keeps insertion order, and `sorted` puts it back.
    #[test]
    fn the_document_is_the_same_in_any_key_order() {
        let mut forward = Map::new();
        let mut backward = Map::new();
        for key in ["b", "a", "c"] {
            forward.insert(key.to_string(), json!({ "z": 1, "y": [{ "q": 2, "p": 3 }] }));
        }
        for key in ["c", "a", "b"] {
            let mut inner = Map::new();
            inner.insert("y".into(), json!([{ "p": 3, "q": 2 }]));
            inner.insert("z".into(), json!(1));
            backward.insert(key.to_string(), Value::Object(inner));
        }
        let (forward, backward) = (sorted(Value::Object(forward)), sorted(Value::Object(backward)));
        assert_eq!(forward.to_string(), backward.to_string());
        assert_eq!(
            forward.to_string(),
            r#"{"a":{"y":[{"p":3,"q":2}],"z":1},"b":{"y":[{"p":3,"q":2}],"z":1},"c":{"y":[{"p":3,"q":2}],"z":1}}"#
        );
        assert!(keys_sorted(&openapi()), "the OpenAPI document's keys are sorted");
    }
}
