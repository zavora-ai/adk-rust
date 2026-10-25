//! The MCP Server Card (spec 3.5 §7.9, §11 "Interoperability"): the cards `McpServer` produces
//! validate against the SEP-2127 schema, vendored under `tests/fixtures/mcp_server_card` with
//! its source, and agree with what the server reports once connected.

use std::sync::Arc;

use adk_core::Url;
use adk_tool::FunctionTool;
use adk_tool::mcp::rmcp::ServerHandler;
use adk_tool::mcp::server::{
    AI_CATALOG_SPEC_VERSION, AiCatalog, AiCatalogEntry, AiCatalogHost, IconTheme, McpServer,
    SERVER_CARD_MEDIA_TYPE, ServerCard, ServerCardHeader, ServerCardIcon, ServerCardInput,
    ServerCardRepository,
};
use proptest::prelude::*;
use serde_json::{Value, json};

const SCHEMA: &str = include_str!("fixtures/mcp_server_card/schema.json");

/// The validator for `#/$defs/ServerCard`, with `format` checked as the upstream script does.
fn card_validator() -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let card = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": schema["$defs"],
        "$ref": "#/$defs/ServerCard",
    });
    jsonschema::options().should_validate_formats(true).build(&card).unwrap()
}

fn errors(validator: &jsonschema::Validator, card: &Value) -> Vec<String> {
    validator.iter_errors(card).map(|error| error.to_string()).collect()
}

fn fixture(path: &str) -> Value {
    let full = format!("{}/tests/fixtures/mcp_server_card/{path}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(full).unwrap()).unwrap()
}

fn full_server() -> McpServer {
    McpServer::builder("com.example/solar", "1.4.0-rc.1")
        .description("Quotes, bookings and orders for Example Solar")
        .title("Example Solar")
        .website_url("https://example.com/agents")
        .repository(
            ServerCardRepository::new("https://github.com/example/solar-site", "github")
                .with_subfolder("edge")
                .with_id("123456"),
        )
        .icon(
            ServerCardIcon::new("https://example.com/icon.png")
                .with_mime_type("image/png")
                .with_sizes(["48x48", "96x96"])
                .with_theme(IconTheme::Light),
        )
        .tool(Arc::new(
            FunctionTool::new("get_quote", "Quotes an installation", |_ctx, _args| async {
                Ok(json!({}))
            })
            .with_read_only(true),
        ))
        .build()
        .unwrap()
}

fn endpoint() -> Url {
    Url::parse("https://agent.example.com/mcp").unwrap()
}

#[test]
fn the_vendored_examples_validate_as_upstream_expects() {
    let validator = card_validator();
    for name in ["minimal", "templated-remote"] {
        let card = fixture(&format!("valid/{name}.json"));
        assert_eq!(errors(&validator, &card), Vec::<String>::new(), "valid/{name}");
    }
    for name in [
        "bad-name-pattern",
        "date-versioned-schema",
        "missing-name",
        "missing-schema",
        "wrong-schema-name",
    ] {
        assert!(!validator.is_valid(&fixture(&format!("invalid/{name}.json"))), "invalid/{name}");
    }
}

#[test]
fn the_valid_examples_round_trip_through_the_card_type() {
    for name in ["minimal", "templated-remote"] {
        let original = fixture(&format!("valid/{name}.json"));
        let card: ServerCard = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(serde_json::to_value(&card).unwrap(), original, "{name}");
    }
}

#[test]
fn a_full_card_validates_against_the_schema() {
    let auth = ServerCardHeader::new("Authorization")
        .with_description("Bearer token the site owner issued")
        .with_value("Bearer {token}")
        .with_variable("token", ServerCardInput::new().required().secret())
        .required()
        .secret();
    let card = full_server().server_card(&endpoint(), &[auth]);
    let value = serde_json::to_value(&card).unwrap();
    assert_eq!(errors(&card_validator(), &value), Vec::<String>::new(), "{value:#}");
    assert_eq!(value["remotes"][0]["type"], "streamable-http");
    assert_eq!(value["remotes"][0]["headers"][0]["name"], "Authorization");
    assert_eq!(value["icons"][0]["theme"], "light");
    assert!(value.get("tools").is_none(), "a card lists no primitives");
}

#[test]
fn the_card_agrees_with_what_a_connected_client_sees() {
    let server = full_server();
    let card = server.server_card(&endpoint(), &[]);
    let info = server.get_info().server_info;
    assert_eq!(
        (Some(info.name), Some(info.version), info.title, info.description, info.website_url),
        (
            Some(card.name.clone()),
            Some(card.version.clone()),
            card.title.clone(),
            Some(card.description.clone()),
            card.website_url.clone(),
        )
    );
    let versions: Vec<String> =
        server.supported_protocol_versions().iter().map(ToString::to_string).collect();
    let remote = &card.remotes.as_ref().unwrap()[0];
    assert_eq!(remote.supported_protocol_versions.as_ref(), Some(&versions));
    assert_eq!(remote.url, "https://agent.example.com/mcp");
}

#[test]
fn the_catalog_lists_the_card_by_url() {
    let server = full_server();
    let card_url = Url::parse("https://agent.example.com/mcp/server-card").unwrap();
    let catalog = AiCatalog::new([server.ai_catalog_entry(&card_url)])
        .with_host(AiCatalogHost::new("Example Solar").with_identifier("did:web:example.com"));
    assert_eq!(
        serde_json::to_value(&catalog).unwrap(),
        json!({
            "specVersion": AI_CATALOG_SPEC_VERSION,
            "host": { "displayName": "Example Solar", "identifier": "did:web:example.com" },
            "entries": [{
                "identifier": "urn:air:example.com:mcp:solar",
                "type": SERVER_CARD_MEDIA_TYPE,
                "url": "https://agent.example.com/mcp/server-card",
            }],
        })
    );
}

#[test]
fn an_inline_catalog_entry_carries_a_valid_card() {
    let card = full_server().server_card(&endpoint(), &[]);
    let entry = AiCatalogEntry::server_card_inline("urn:air:example.com:mcp:solar", &card).unwrap();
    let value = serde_json::to_value(&entry).unwrap();
    assert!(value.get("url").is_none());
    assert_eq!(errors(&card_validator(), &value["data"]), Vec::<String>::new());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Whatever name, version and description the builder accepts, the card validates.
    #[test]
    fn every_card_the_builder_accepts_validates(
        namespace in "[a-z][a-z0-9-]{0,20}(\\.[a-z][a-z0-9-]{0,20}){0,3}",
        server in "[a-zA-Z0-9][a-zA-Z0-9._-]{0,40}",
        major in 0u32..100,
        minor in 0u32..100,
        pre in proptest::option::of("[a-z]{1,8}\\.[0-9]{1,3}"),
        description in "[ -~]{1,100}",
        title in proptest::option::of("[ -~]{1,100}"),
    ) {
        let version = match pre {
            Some(pre) => format!("{major}.{minor}.0-{pre}"),
            None => format!("{major}.{minor}.0"),
        };
        let mut builder = McpServer::builder(format!("{namespace}/{server}"), version)
            .description(description);
        if let Some(title) = title {
            builder = builder.title(title);
        }
        let card = builder.build().unwrap().server_card(&endpoint(), &[]);
        let value = serde_json::to_value(&card).unwrap();
        prop_assert_eq!(errors(&card_validator(), &value), Vec::<String>::new());
    }

    /// A name outside the card schema's pattern never builds.
    #[test]
    fn names_outside_the_schema_never_build(name in "[a-z./ ]{0,12}") {
        let pattern_ok = name.len() >= 3
            && name.split_once('/').is_some_and(|(namespace, server)| {
                !namespace.is_empty()
                    && !server.is_empty()
                    && !server.contains('/')
                    && !namespace.contains(' ')
                    && !server.contains(' ')
            });
        let built = McpServer::builder(name.clone(), "1.0.0").description("d").build();
        prop_assert_eq!(built.is_ok(), pattern_ok, "{}", name);
    }
}
