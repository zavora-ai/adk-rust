//! The Runs tab's OpenAPI document and TypeScript types (spec 3.3 §12, feature `workflow`): a
//! document of its own, so the engine's document stays the same in every `web` build.
//! `scripts/check-surface-api.sh` writes both under `adk-surface/web/src/generated/`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use schemars::generate::Contract;
use serde_json::{Map, Value, json};

use super::{
    answer, clean_descriptions, doc_comment, errors, generator, object_body, path_param, query,
    read, schema_ref, sorted, ts_type, write,
};
use crate::web::workflow::{WorkflowChanged, paths};
use crate::workflow::{
    ForkOutcome, ForkRequest, GraphsResponse, RunsResponse, StepDetail, WorkflowRun,
};

/// The document (spec 3.3 §12).
pub(crate) fn openapi() -> Value {
    let mut out = generator(Contract::Serialize);
    let graphs = schema_ref::<GraphsResponse>(&mut out);
    let runs = schema_ref::<RunsResponse>(&mut out);
    let run = schema_ref::<WorkflowRun>(&mut out);
    let step = schema_ref::<StepDetail>(&mut out);
    let outcome = schema_ref::<ForkOutcome>(&mut out);
    let changed = schema_ref::<WorkflowChanged>(&mut out);
    let mut schemas = out.take_definitions(true);

    let mut input = generator(Contract::Deserialize);
    let fork = schema_ref::<ForkRequest>(&mut input);
    for (name, schema) in input.take_definitions(true) {
        schemas.entry(name).or_insert(schema);
    }

    let with = |path: &str| format!("/api{path}");
    let graph = query(
        "graph",
        "The graph agent whose runs to read; the one with checkpoints on the session, else the only one",
        json!({ "type": "string" }),
    );
    let mut routes = BTreeMap::new();
    routes.insert(
        with(paths::GRAPHS),
        read("Every graph agent observed (spec 3.3 §12)", json!([]), answer("The graphs", graphs)),
    );
    routes.insert(
        with(paths::RUNS),
        read(
            "Every run of the caller's session, from the checkpoints' records only",
            json!([path_param("session"), graph.clone()]),
            answer("The runs, oldest first", runs),
        ),
    );
    routes.insert(
        with(paths::RUN),
        read(
            "The latest run of the caller's session: each node's state, the steps, the lineage and forks, and each waiting node's decision id from the inbox",
            json!([path_param("session"), graph]),
            answer("The run", run),
        ),
    );
    routes.insert(
        with(paths::CHECKPOINT),
        read(
            "One step, read from one checkpoint: its nodes, what changed, its state and what a fork from it would do. Reading changes nothing",
            json!([path_param("session"), path_param("id")]),
            answer("The step", step),
        ),
    );
    routes.insert(
        with(paths::FEED),
        json!({ "get": {
            "summary": "The session's run changes as server-sent events: a `changed` frame, with `id` = the newest checkpoint version, whenever the thread writes a checkpoint; resumes after `Last-Event-ID`",
            "parameters": [
                path_param("session"),
                { "name": "Last-Event-ID", "in": "header", "required": false, "schema": { "type": "string" } },
            ],
            "responses": errors({
                let mut ok = Map::new();
                ok.insert("200".into(), json!({
                    "description": "`text/event-stream` of WorkflowChanged frames",
                    "content": { "text/event-stream": { "schema": changed } },
                }));
                ok
            }),
        } }),
    );
    routes.insert(
        with(paths::FORK),
        write(
            "post",
            "Forks the caller's session at a completed step into a new session, with edits; starts it when asked. Refused from a paired device (403), while a decision of the session waits (409), and for a pause, a step with nothing after it or an undeclared key (400)",
            json!([path_param("session")]),
            Some(fork),
            ("200", answer("The fork", outcome.clone())),
        ),
    );
    routes.insert(
        with(paths::START),
        write(
            "post",
            "Starts a fork made and not started, such as one made while everything was stopped",
            json!([path_param("session")]),
            None,
            ("200", answer("The fork", outcome)),
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
            "title": "adk-surface workflow",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "The workflow observer on the web home's Runs tab (spec 3.3 §12): graphs, runs, steps and forks. Generated from the Rust types; do not edit.",
        },
        "paths": routes,
        "components": { "schemas": schemas },
    }))
}

/// TypeScript declarations of every schema in [`openapi`].
pub(crate) fn typescript() -> String {
    let document = openapi();
    let mut out = String::from(
        "// Generated from adk-surface's workflow view types by `scripts/check-surface-api.sh --write`.\n\
         // Do not edit: CI fails when this file drifts from the Rust source (spec 3.3 §12).\n",
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
