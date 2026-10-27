//! The screen actions' OpenAPI document and TypeScript types (spec 3.3 §2 item 7, §6;
//! features `verb-ui` and `web`): a document of its own, so the engine's document stays the
//! same in every `web` build. `scripts/check-surface-api.sh` writes both under
//! `adk-surface/web/src/generated/`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use schemars::generate::Contract;
use serde_json::{Value, json};

use super::{
    answer, clean_descriptions, doc_comment, generator, object_body, query, read, schema_ref,
    sorted, ts_type, write,
};
use crate::origin::SurfaceKind;
use crate::verbs::VerbDone;
use crate::web::verbs::{VerbCallRequest, VerbsResponse, paths};

/// The document.
pub(crate) fn openapi() -> Value {
    let mut out = generator(Contract::Serialize);
    let verbs = schema_ref::<VerbsResponse>(&mut out);
    let done = schema_ref::<VerbDone>(&mut out);
    let mut schemas = out.take_definitions(true);

    let mut input = generator(Contract::Deserialize);
    let request = schema_ref::<VerbCallRequest>(&mut input);
    let surface = schema_ref::<SurfaceKind>(&mut input);
    for (name, schema) in input.take_definitions(true) {
        schemas.entry(name).or_insert(schema);
    }

    let with = |path: &str| format!("/api{path}");
    let mut routes = BTreeMap::new();
    routes.insert(
        with(paths::VERBS),
        read(
            "The screen actions this home offers, and the caller's Coming up with the times each item may move to (spec 3.3 §2 item 7)",
            json!([query("surface", "The surface asking; chat and the tray list nothing to act on", surface)]),
            answer("The catalogue and Coming up", verbs),
        ),
    );
    routes.insert(
        with(paths::CALL),
        write(
            "post",
            "Takes a screen action as the authenticated person, through the governed path; answers once it ran. A declared voice or agent origin, or the voice or chat surface, answers 400",
            json!([]),
            Some(request),
            ("200", answer("What it did", done)),
        ),
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
            "title": "adk-surface screen actions",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Screen actions people and agents share (spec 3.3 §2 item 7): the catalogue, Coming up and taking an action. Generated from the Rust types; do not edit.",
        },
        "paths": routes,
        "components": { "schemas": schemas },
    }))
}

/// TypeScript declarations of every schema in [`openapi`].
pub(crate) fn typescript() -> String {
    let document = openapi();
    let mut out = String::from(
        "// Generated from adk-surface's screen-action types by `scripts/check-surface-api.sh --write`.\n\
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
