//! Group H: the agent registry authenticates every request and lets only a card's owner
//! change it.
//!
//! A registry service (`registry_router` nested under `/api`) and a `ServerBuilder` serving a
//! `claude-sonnet-5-5` `LlmAgent` share one `RequestContextExtractor`, a token table naming
//! `alice` and `bob`. Alice registers a card for the agent, Bob discovers it, and Alice runs
//! the agent at the card's endpoint. The other scenarios send tokens the extractor refuses,
//! act as Bob on Alice's cards, or change a card stored without an owner.

use std::sync::Arc;
use std::time::Duration;

use adk_core::{Llm, RequestContext, SingleAgentLoader};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::registry::{
    AgentCard, AgentRegistryStore, InMemoryAgentRegistryStore, registry_router,
};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use axum::Router;
use axum::http::request::Parts;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use crate::{
    Kind, Outcome, errored, event_text, http, outcome, plain_agent, run_body, serve, skip, truncate,
};

const ANTHROPIC_MODEL: &str = "claude-sonnet-5-5";
const ALICE: &str = "Bearer alice-token";
const BOB: &str = "Bearer bob-token";
const DUMMY: &str = "Bearer anything-at-all";

/// `alice-token` and `bob-token` name their users, `nobody-token` names an empty user ID,
/// `broken-token` fails extraction, and any other bearer token is invalid.
struct TokenTable;

#[async_trait::async_trait]
impl RequestContextExtractor for TokenTable {
    async fn extract(&self, parts: &Parts) -> Result<RequestContext, RequestContextError> {
        let token = parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or(RequestContextError::MissingAuth)?;
        let user_id = match token {
            "alice-token" => "alice",
            "bob-token" => "bob",
            "nobody-token" => "",
            "broken-token" => {
                return Err(RequestContextError::ExtractionFailed("key set unavailable".into()));
            }
            _ => return Err(RequestContextError::InvalidToken("unknown token".to_string())),
        };
        Ok(RequestContext {
            user_id: user_id.to_string(),
            scopes: vec![],
            metadata: Default::default(),
        })
    }
}

/// The registry service: `registry_router` over `store` under `/api`, authenticated by
/// `extractor`.
fn registry_app(
    store: Arc<InMemoryAgentRegistryStore>,
    extractor: Arc<dyn RequestContextExtractor>,
) -> Router {
    Router::new().nest("/api", registry_router(store, extractor))
}

fn card(name: &str, version: &str, description: &str, endpoint: &str) -> Value {
    json!({
        "name": name,
        "version": version,
        "description": description,
        "tags": ["support"],
        "endpointUrl": endpoint,
        "capabilities": ["text"],
        "inputModes": ["text"],
        "outputModes": ["text"],
        "createdAt": "2026-10-28T00:00:00Z",
    })
}

struct Registry {
    base: String,
    client: reqwest::Client,
}

impl Registry {
    async fn send(
        &self,
        method: Method,
        path: &str,
        auth: &str,
        body: Option<&Value>,
    ) -> anyhow::Result<(StatusCode, Value)> {
        let mut request = self
            .client
            .request(method, format!("{}/api{path}", self.base))
            .header("authorization", auth);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        let status = response.status();
        Ok((status, response.json().await.unwrap_or(Value::Null)))
    }

    /// Registers `card` as Alice, failing the scenario when the registry refuses it.
    async fn register_as_alice(&self, card: &Value) -> anyhow::Result<()> {
        let (status, body) = self.send(Method::POST, "/agents", ALICE, Some(card)).await?;
        anyhow::ensure!(status == StatusCode::CREATED, "alice registering: HTTP {status} {body}");
        Ok(())
    }

    /// The card's version and owner as Alice reads them, or the HTTP status of the read.
    async fn read(&self, name: &str) -> anyhow::Result<Result<(String, String), StatusCode>> {
        Ok(self.read_full(name).await?.map(|body| {
            let field = |key: &str| body[key].as_str().unwrap_or("-").to_string();
            (field("version"), field("owner"))
        }))
    }

    /// The card as Alice reads it, or the HTTP status of the read.
    async fn read_full(&self, name: &str) -> anyhow::Result<Result<Value, StatusCode>> {
        let (status, body) =
            self.send(Method::GET, &format!("/agents/{name}"), ALICE, None).await?;
        Ok(if status == StatusCode::OK { Ok(body) } else { Err(status) })
    }
}

pub(crate) async fn group_h(out: &mut Vec<Outcome>) {
    let extractor: Arc<dyn RequestContextExtractor> = Arc::new(TokenTable);
    let store = Arc::new(InMemoryAgentRegistryStore::new());
    let registry = match serve(|_| registry_app(store.clone(), extractor.clone())).await {
        Ok((base, task)) => (Registry { base, client: http() }, task),
        Err(error) => {
            out.push(errored("H1 registry-owner-registers-and-runs", Kind::Live, error));
            return;
        }
    };
    let (registry, registry_task) = registry;

    h1_register_and_run(&registry, extractor, out).await;

    // H2: a dummy token on every route.
    let dummy = async {
        let target = card("dummy_target", "1.0.0", "Alice's card", "http://127.0.0.1:9/api/run");
        registry.register_as_alice(&target).await?;
        let probe = card("dummy_probe", "1.0.0", "registered with a dummy token", "-");
        let changed = card("dummy_target", "6.6.6", "changed with a dummy token", "-");
        let mut statuses = Vec::new();
        for (method, path, body) in [
            (Method::POST, "/agents", Some(&probe)),
            (Method::GET, "/agents", None),
            (Method::GET, "/agents/dummy_target", None),
            (Method::PUT, "/agents/dummy_target", Some(&changed)),
            (Method::DELETE, "/agents/dummy_target", None),
        ] {
            let (status, _) = registry.send(method.clone(), path, DUMMY, body).await?;
            statuses.push(format!("{method} {}", status.as_u16()));
        }
        let all_refused = statuses.iter().all(|status| status.ends_with(" 401"));
        let target = registry.read("dummy_target").await?;
        Ok::<_, anyhow::Error>((all_refused && target.is_ok(), statuses, target))
    }
    .await;
    out.push(match dummy {
        Ok((pass, statuses, target)) => outcome(
            "H2 registry-dummy-token-refused",
            Kind::Local,
            pass,
            format!("{}; Alice's card afterwards: {target:?}", statuses.join(", ")),
        ),
        Err(error) => errored("H2 registry-dummy-token-refused", Kind::Local, error),
    });

    // H3: Bob changes Alice's card — with PUT, or by deleting it and registering his own.
    let update = async {
        registry.register_as_alice(&card("update_target", "1.0.0", "Alice's card", "-")).await?;
        let bobs = card("update_target", "6.6.6", "Bob's card", "http://attacker.example/run");
        let put = registry.send(Method::PUT, "/agents/update_target", BOB, Some(&bobs)).await?.0;
        let delete = registry.send(Method::DELETE, "/agents/update_target", BOB, None).await?.0;
        let post = registry.send(Method::POST, "/agents", BOB, Some(&bobs)).await?.0;
        let after = registry.read("update_target").await?;
        let kept = after.as_ref().is_ok_and(|(version, _)| version == "1.0.0");
        Ok::<_, anyhow::Error>((
            kept,
            format!(
                "PUT {}, DELETE {}, POST {}; card afterwards: {after:?}",
                put.as_u16(),
                delete.as_u16(),
                post.as_u16()
            ),
        ))
    }
    .await;
    out.push(match update {
        Ok((pass, detail)) => {
            outcome("H3 registry-other-user-cannot-update", Kind::Local, pass, detail)
        }
        Err(error) => errored("H3 registry-other-user-cannot-update", Kind::Local, error),
    });

    // H4: Bob deletes Alice's card.
    let delete = async {
        registry.register_as_alice(&card("delete_target", "1.0.0", "Alice's card", "-")).await?;
        let (status, body) =
            registry.send(Method::DELETE, "/agents/delete_target", BOB, None).await?;
        let after = registry.read("delete_target").await?;
        Ok::<_, anyhow::Error>((
            !status.is_success() && after.is_ok(),
            format!("DELETE {} {body}; card afterwards: {after:?}", status.as_u16()),
        ))
    }
    .await;
    out.push(match delete {
        Ok((pass, detail)) => {
            outcome("H4 registry-other-user-cannot-delete", Kind::Local, pass, detail)
        }
        Err(error) => errored("H4 registry-other-user-cannot-delete", Kind::Local, error),
    });

    // H5 and H6: Alice registers a card naming another owner in its body, registers its name
    // again, updates it, and deletes it.
    let owner = async {
        let mut first = card("owner_target", "1.0.0", "Alice's card", "-");
        first["owner"] = json!("mallory");
        registry.register_as_alice(&first).await?;
        let registered = registry.read("owner_target").await?;
        let again = registry.send(Method::POST, "/agents", ALICE, Some(&first)).await?.0;
        let newer = card("owner_target", "1.1.0", "Alice's card, revised", "-");
        let put = registry.send(Method::PUT, "/agents/owner_target", ALICE, Some(&newer)).await?.0;
        let updated = registry.read_full("owner_target").await?;
        let delete = registry.send(Method::DELETE, "/agents/owner_target", ALICE, None).await?.0;
        let deleted = registry.read("owner_target").await?;
        let put_gone =
            registry.send(Method::PUT, "/agents/owner_target", ALICE, Some(&newer)).await?.0;
        let delete_gone =
            registry.send(Method::DELETE, "/agents/owner_target", ALICE, None).await?.0;
        Ok::<_, anyhow::Error>((
            registered,
            again,
            put,
            updated,
            delete,
            deleted,
            put_gone,
            delete_gone,
        ))
    }
    .await;
    match owner {
        Ok((registered, again, put, updated, delete, deleted, put_gone, delete_gone)) => {
            let view = updated.as_ref().map(|card| {
                (
                    card["version"].as_str().unwrap_or("-").to_string(),
                    card["owner"].as_str().unwrap_or("-").to_string(),
                    card["createdAt"].as_str().unwrap_or("-").to_string(),
                    card["updatedAt"].is_string(),
                )
            });
            let expected = (
                "1.1.0".to_string(),
                "alice".to_string(),
                "2026-10-28T00:00:00Z".to_string(),
                true,
            );
            out.push(outcome(
                "H5 registry-owner-updates",
                Kind::Local,
                registered.as_ref().is_ok_and(|(_, owner)| owner == "alice")
                    && again == StatusCode::CONFLICT
                    && put == StatusCode::OK
                    && view.as_ref().is_ok_and(|view| *view == expected),
                format!(
                    "registered with body owner mallory: {registered:?}; POST again {}; PUT {}; \
                     (version, owner, createdAt, updatedAt set) afterwards: {view:?}",
                    again.as_u16(),
                    put.as_u16()
                ),
            ));
            out.push(outcome(
                "H6 registry-owner-deletes",
                Kind::Local,
                delete == StatusCode::NO_CONTENT
                    && deleted == Err(StatusCode::NOT_FOUND)
                    && put_gone == StatusCode::NOT_FOUND
                    && delete_gone == StatusCode::NOT_FOUND,
                format!(
                    "DELETE {}; then GET {deleted:?}, PUT {}, DELETE {}",
                    delete.as_u16(),
                    put_gone.as_u16(),
                    delete_gone.as_u16()
                ),
            ));
        }
        Err(error) => {
            out.push(errored("H5 registry-owner-updates", Kind::Local, error));
            out.push(skip("H6 registry-owner-deletes", Kind::Local, "H5 setup failed"));
        }
    }

    // H7: an extractor that names an empty user, or fails, admits no one.
    let refused = async {
        let probe = card("identity_probe", "1.0.0", "registered by no one", "-");
        let empty =
            registry.send(Method::POST, "/agents", "Bearer nobody-token", Some(&probe)).await?;
        let broken =
            registry.send(Method::POST, "/agents", "Bearer broken-token", Some(&probe)).await?;
        let after = registry.read("identity_probe").await?;
        Ok::<_, anyhow::Error>((empty, broken, after))
    }
    .await;
    out.push(match refused {
        Ok(((empty, empty_body), (broken, broken_body), after)) => outcome(
            "H7 registry-refused-identities",
            Kind::Local,
            empty == StatusCode::UNAUTHORIZED
                && broken == StatusCode::INTERNAL_SERVER_ERROR
                && after == Err(StatusCode::NOT_FOUND),
            format!(
                "empty user ID: {} {empty_body}; extraction failure: {} {broken_body}; card: {after:?}",
                empty.as_u16(),
                broken.as_u16()
            ),
        ),
        Err(error) => errored("H7 registry-refused-identities", Kind::Local, error),
    });

    // H8: a card stored without an owner, as a store written before cards had owners holds.
    let legacy = async {
        let stored: AgentCard =
            serde_json::from_value(card("legacy_agent", "1.0.0", "stored before owners", "-"))?;
        store.insert(stored).await?;
        let read = registry.send(Method::GET, "/agents/legacy_agent", BOB, None).await?.0;
        let changed = card("legacy_agent", "2.0.0", "claimed", "-");
        let mut statuses = vec![format!("Bob GET {}", read.as_u16())];
        for (who, auth) in [("Alice", ALICE), ("Bob", BOB)] {
            let put =
                registry.send(Method::PUT, "/agents/legacy_agent", auth, Some(&changed)).await?.0;
            let delete = registry.send(Method::DELETE, "/agents/legacy_agent", auth, None).await?.0;
            let post = registry.send(Method::POST, "/agents", auth, Some(&changed)).await?.0;
            statuses.push(format!(
                "{who} PUT {} DELETE {} POST {}",
                put.as_u16(),
                delete.as_u16(),
                post.as_u16()
            ));
        }
        let after = registry.read("legacy_agent").await?;
        Ok::<_, anyhow::Error>((read, statuses, after))
    }
    .await;
    out.push(match legacy {
        Ok((read, statuses, after)) => outcome(
            "H8 registry-card-without-owner-read-only",
            Kind::Local,
            read == StatusCode::OK
                && statuses[1..].iter().all(|line| line.ends_with("PUT 403 DELETE 403 POST 409"))
                && after == Ok(("1.0.0".to_string(), "-".to_string())),
            format!("{}; card afterwards: {after:?}", statuses.join(", ")),
        ),
        Err(error) => errored("H8 registry-card-without-owner-read-only", Kind::Local, error),
    });

    registry_task.abort();
}

/// H1: Alice registers a card for a live agent, Bob discovers it, and Alice runs the agent
/// at the card's endpoint.
async fn h1_register_and_run(
    registry: &Registry,
    extractor: Arc<dyn RequestContextExtractor>,
    out: &mut Vec<Outcome>,
) {
    const ID: &str = "H1 registry-owner-registers-and-runs";
    let Some(key) = std::env::var("ANTHROPIC_API_KEY").ok().filter(|key| !key.trim().is_empty())
    else {
        out.push(skip(ID, Kind::Live, "ANTHROPIC_API_KEY not set"));
        return;
    };
    let app = "support_agent";
    let run = async {
        let model: Arc<dyn Llm> =
            Arc::new(AnthropicClient::new(AnthropicConfig::new(key, ANTHROPIC_MODEL).with_max_tokens(64))?);
        let agent = plain_agent(
            app,
            "You are support_agent. Reply with exactly the text the user asks for and nothing else.",
            model,
        )?;
        let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let (agent_base, agent_task) = serve(|_| {
            let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(agent)), sessions)
                .with_request_context(extractor)
                .with_request_timeout(Duration::from_secs(90));
            ServerBuilder::new(config).build()
        })
        .await?;

        let description = format!("{ANTHROPIC_MODEL} LlmAgent that answers support questions");
        registry
            .register_as_alice(&card(app, "1.0.0", &description, &format!("{agent_base}/api/run")))
            .await?;

        // Bob, another authenticated caller, discovers the card.
        let (listed, cards) = registry.send(Method::GET, "/agents?tag=support", BOB, None).await?;
        let found = cards
            .as_array()
            .and_then(|cards| cards.iter().find(|card| card["name"] == app))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Bob's listing (HTTP {listed}) has no {app}"))?;
        let endpoint = found["endpointUrl"].as_str().unwrap_or_default().to_string();

        let response = registry
            .client
            .post(&endpoint)
            .header("authorization", ALICE)
            .json(&run_body(app, "alice", "s-registry", "Reply with exactly: registry ok"))
            .send()
            .await?;
        let status = response.status();
        let events: Vec<adk_core::Event> = response.json().await.unwrap_or_default();
        agent_task.abort();
        Ok::<_, anyhow::Error>((status, event_text(&events), found))
    }
    .await;
    out.push(match run {
        Ok((status, text, found)) => outcome(
            ID,
            Kind::Live,
            status.is_success() && text.to_lowercase().contains("registry ok"),
            format!(
                "card owner {}, run HTTP {status}, {ANTHROPIC_MODEL} said {:?}",
                found["owner"].as_str().unwrap_or("(none)"),
                truncate(&text, 40)
            ),
        ),
        Err(error) => errored(ID, Kind::Live, error),
    });
}
