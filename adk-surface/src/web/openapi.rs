//! The OpenAPI document of the web API, generated from the Rust types, and the TypeScript
//! types generated from it (spec 3.3 §6). `scripts/check-surface-api.sh` writes both under
//! `adk-surface/web/src/generated/` and CI fails when they drift from the Rust source.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use schemars::JsonSchema;
use schemars::generate::{Contract, SchemaGenerator, SchemaSettings};
use serde_json::{Map, Value, json};

use super::{
    AgentsResponse, CommandRequest, DecisionsResponse, SurfaceQuery, ThreadResponse, paths,
};
use crate::engine::{CommandOutcome, FeedFrame};
use crate::model::{Briefing, DecisionCard, FeedEntry, StatusLine};

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
    let mut schemas = out.take_definitions(true);

    let mut input = generator(Contract::Deserialize);
    let request = schema_ref::<CommandRequest>(&mut input);
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
        with(paths::AGENTS),
        read("The caller's agents (spec 3.3 §8.1)", json!([]), answer("The roster", agents)),
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
                ok.insert("200".into(), answer("What the command did", outcome));
                ok.insert("422".into(), problem());
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
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "adk-surface",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "The engine every agent surface runs on (spec 3.3 §6): decisions, threads, agents, the status line, the briefing and commands. Generated from the Rust types; do not edit.",
        },
        "paths": routes,
        "components": { "schemas": schemas },
    })
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
