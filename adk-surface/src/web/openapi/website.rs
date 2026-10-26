//! The website API's OpenAPI document and TypeScript types (spec 3.5 §8.2, feature
//! `website`): a document of its own, so the engine's document stays the same in every `web`
//! build. `scripts/check-surface-api.sh` writes both under `adk-surface/web/src/generated/`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use adk_website::ui::charter::CharterCommand;
use adk_website::ui::setup::ClaimRequest;
use adk_website::ui::{
    ClaimedView, DraftView, EditorCommand, EditorHome, EditorOutcome, OnboardingCommand,
    OnboardingOutcome, OnboardingView, ReviewCommand, ReviewPage, SetupView, SiteCommand,
    SiteOutcome, SiteViewModel,
};
use schemars::generate::Contract;
use serde_json::{Value, json};

use super::{
    answer, clean_descriptions, doc_comment, generator, object_body, path_param, query, read,
    schema_ref, sorted, ts_type, write,
};
use crate::web::website::{CharterResponse, UploadRequest, WebsiteHome, paths};

/// The document (spec 3.5 §8.2).
pub(crate) fn openapi() -> Value {
    let mut out = generator(Contract::Serialize);
    let home = schema_ref::<WebsiteHome>(&mut out);
    let setup = schema_ref::<SetupView>(&mut out);
    let claimed = schema_ref::<ClaimedView>(&mut out);
    let onboarding = schema_ref::<OnboardingView>(&mut out);
    let onboarding_outcome = schema_ref::<OnboardingOutcome>(&mut out);
    let charter = schema_ref::<CharterResponse>(&mut out);
    let site = schema_ref::<SiteViewModel>(&mut out);
    let site_outcome = schema_ref::<SiteOutcome>(&mut out);
    let review = schema_ref::<ReviewPage>(&mut out);
    let editor = schema_ref::<EditorHome>(&mut out);
    let editor_outcome = schema_ref::<EditorOutcome>(&mut out);
    let draft = schema_ref::<DraftView>(&mut out);
    let mut schemas = out.take_definitions(true);

    let mut input = generator(Contract::Deserialize);
    let claim = schema_ref::<ClaimRequest>(&mut input);
    let onboarding_command = schema_ref::<OnboardingCommand>(&mut input);
    let charter_command = schema_ref::<CharterCommand>(&mut input);
    let site_command = schema_ref::<SiteCommand>(&mut input);
    let review_command = schema_ref::<ReviewCommand>(&mut input);
    let editor_command = schema_ref::<EditorCommand>(&mut input);
    let upload = schema_ref::<UploadRequest>(&mut input);
    for (name, schema) in input.take_definitions(true) {
        schemas.entry(name).or_insert(schema);
    }

    let bearer = "Needs the paired device's token as `Authorization: Bearer` (spec 3.5 A-I20)";
    let mut routes = BTreeMap::new();
    let with = |path: &str| format!("/api{path}");
    routes.insert(
        paths::SETUP.to_string(),
        read(
            "The setup screen of a fresh site (spec 3.5 §7.17); a root route, needing no caller",
            json!([]),
            answer("The setup screen", setup),
        ),
    );
    routes.insert(
        paths::CLAIM.to_string(),
        write(
            "post",
            "Claims the setup code from this device (spec 3.5 A-I20); a root route: the code is the credential. The token is shown once",
            json!([]),
            Some(claim),
            ("200", answer("The paired device and its token", claimed)),
        ),
    );
    routes.insert(
        with(paths::ROOT),
        read("The site and the caller's roles in its charter", json!([]), answer("The site", home)),
    );
    let onboarding_route = both(
        read(
            &format!("Onboarding as the owner sees it (spec 3.5 §7.18). {bearer}"),
            json!([]),
            answer("Onboarding", onboarding),
        ),
        write(
            "post",
            &format!("An input to onboarding (spec 3.5 §7.18). {bearer}"),
            json!([]),
            Some(onboarding_command),
            ("200", answer("Onboarding after it", onboarding_outcome)),
        ),
    );
    routes.insert(with(paths::ONBOARDING), onboarding_route);
    routes.insert(
        with(paths::CHARTER),
        both(
            read("The charter under review (spec 3.5 §7.18 stage 5)", json!([]), answer("The charter", charter.clone())),
            write(
                "post",
                "Signs the charter in a role the caller holds (spec 3.5 A-I23); changes go through onboarding",
                json!([]),
                Some(charter_command),
                ("200", answer("The charter after it", charter)),
            ),
        ),
    );
    routes.insert(
        with(paths::SITE),
        both(
            read("The site view (spec 3.5 §7.24)", json!([]), answer("The site view", site)),
            write(
                "post",
                "An owner's decision on the site view: promote or lower a job, release or discard what is held, acknowledge an alarm (spec 3.5 §7.11, §7.8.3)",
                json!([]),
                Some(site_command),
                ("200", answer("The site view after it", site_outcome)),
            ),
        ),
    );
    routes.insert(
        with(paths::REVIEW),
        both(
            read(
                "The weekly review page (spec 3.5 §7.21)",
                json!([query(
                    "week",
                    "The Monday of the week to show",
                    json!({ "type": "string", "format": "date" })
                )]),
                answer("The review page", review.clone()),
            ),
            write(
                "post",
                "Composes a week's review, or sends its summary to the owner's channel",
                json!([]),
                Some(review_command),
                ("200", answer("The review page after it", review)),
            ),
        ),
    );
    routes.insert(
        with(paths::EDITOR),
        both(
            read("The web editor's home (spec 3.5 §7.8.2)", json!([]), answer("The editor", editor)),
            write(
                "post",
                "An editor action: draft, save, preview, publish, approve, correct, withdraw (spec 3.5 §7.8.2, A-I23, A-I24)",
                json!([]),
                Some(editor_command),
                ("200", answer("The editor after it", editor_outcome)),
            ),
        ),
    );
    routes.insert(
        with(paths::DRAFT),
        read("One draft", json!([path_param("id")]), answer("The draft", draft.clone())),
    );
    routes.insert(
        with(paths::MEDIA),
        write(
            "post",
            "One media file through the media pipeline, into the draft (spec 3.5 §7.8.2 \"Media\")",
            json!([path_param("id")]),
            Some(upload),
            ("200", answer("The draft with the file", draft)),
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
            "title": "adk-surface website",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "The website's views on the web home (spec 3.5 §8.2): setup, onboarding, the charter, the site view, the weekly review and the web editor. Generated from the Rust types; do not edit.",
        },
        "paths": routes,
        "components": { "schemas": schemas },
    }))
}

/// A route's `get` and `post` together.
fn both(get: Value, post: Value) -> Value {
    let mut route = get;
    if let (Some(route), Value::Object(post)) = (route.as_object_mut(), post) {
        route.extend(post);
    }
    route
}

/// TypeScript declarations of every schema in [`openapi`].
pub(crate) fn typescript() -> String {
    let document = openapi();
    let mut out = String::from(
        "// Generated from adk-website's view types by `scripts/check-surface-api.sh --write`.\n\
         // Do not edit: CI fails when this file drifts from the Rust source (spec 3.5 §8.2).\n",
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
