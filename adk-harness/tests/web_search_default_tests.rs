//! `web_search` defaults to the search built-in of the provider the harness model's client
//! serves; a configured search provider replaces it (spec 3.1 §7.20, §9.2 I49, §13
//! question 7). The fetch allowlist is never a search filter; `search_domains` is, where
//! the provider takes one (spec 3.1 §7.32, I73, §13 question 8). Scripted models behind a
//! real runner, no network beyond loopback.

#![cfg(all(feature = "runner", feature = "web"))]

use crate::common;

use std::sync::{Arc, Mutex};

use adk_core::{Content, LlmResponse, NativeWebSearch, NativeWebSearchTool, Part, Result};
use adk_harness::HarnessMode;
use adk_harness::web::{
    DeepSeekSearch, ProviderWebTools, SearchDomains, SearchHit, SearchProvider, SearxngSearch,
    WebConfig, WebSearchPath,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

fn web(allow: &[&str]) -> WebConfig {
    WebConfig {
        allowed_domains: allow.iter().map(|d| (*d).to_string()).collect(),
        ..WebConfig::default()
    }
}

fn model(search: Option<NativeWebSearch>, responses: Vec<ScriptedResponse>) -> ScriptedLlm {
    let model = ScriptedLlm::new("scripted", responses);
    match search {
        Some(search) => model.with_native_web_search(search),
        None => model,
    }
}

fn gemini() -> NativeWebSearch {
    NativeWebSearch::new("gemini", NativeWebSearchTool::GoogleSearch, false)
}

fn anthropic() -> NativeWebSearch {
    NativeWebSearch::new("anthropic", NativeWebSearchTool::AnthropicWebSearch, true)
}

fn openai() -> NativeWebSearch {
    NativeWebSearch::new("openai", NativeWebSearchTool::OpenAiWebSearch, true)
}

fn deepseek() -> NativeWebSearch {
    NativeWebSearch::new("deepseek", NativeWebSearchTool::DeepSeekWebSearch, false)
}

fn banner(request: &adk_core::LlmRequest) -> Option<String> {
    request.contents.iter().flat_map(|c| &c.parts).find_map(|part| match part {
        Part::Text { text, .. } if text.contains("Provider-native web tools are live") => {
            Some(text.clone())
        }
        _ => None,
    })
}

#[derive(Debug)]
struct FixedSearch;

#[async_trait::async_trait]
impl SearchProvider for FixedSearch {
    fn name(&self) -> &str {
        "fixed"
    }

    async fn search(
        &self,
        query: &str,
        _limit: usize,
        _site: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        Ok(vec![SearchHit {
            title: query.into(),
            url: "https://docs.rs/adk-core".into(),
            snippet: "The core traits.".into(),
        }])
    }
}

/// A fetch allowlist of `internal.example` and a search filter of `docs.rs`.
fn web_with_filter(filter: SearchDomains) -> WebConfig {
    WebConfig { search_domains: Some(filter), ..web(&["internal.example"]) }
}

fn only_docs_rs() -> SearchDomains {
    SearchDomains::Allowed(vec!["docs.rs".into()])
}

#[tokio::test]
async fn each_providers_model_declares_its_search_by_default_and_none_when_the_run_starts_in_plan()
{
    let cases = [
        (gemini(), "google_search", None),
        (
            anthropic(),
            "web_search",
            Some(("/x-adk-anthropic-tool/allowed_domains", json!(["docs.rs"]))),
        ),
        (
            openai(),
            "openai_web_search",
            Some(("/x-adk-openai-tool/filters/allowed_domains", json!(["docs.rs"]))),
        ),
        (deepseek(), "deepseek_web_search", None),
    ];
    for (search, tool, forwarded) in cases {
        for (mode, declared) in [
            (HarnessMode::Ask, true),
            (HarnessMode::Auto, true),
            (HarnessMode::ReadOnly, false),
            (HarnessMode::Plan, false),
        ] {
            let f = common::fixture_on(model(Some(search), vec![done()]), |b| {
                b.mode(mode).web(web_with_filter(only_docs_rs()))
            })
            .await;
            f.send("hello").await;
            let request = f.model.requests().into_iter().next().unwrap();
            assert_eq!(
                request.tools.contains_key(tool),
                declared,
                "{} in {mode}: {:?}",
                search.provider,
                request.tools.keys()
            );
            if !declared {
                assert!(banner(&request).is_none());
                assert!(f.records("harness.web.provider_enabled").is_empty());
                continue;
            }
            let banner = banner(&request).unwrap();
            match &forwarded {
                Some((pointer, domains)) => {
                    assert_eq!(
                        request.tools[tool].pointer(pointer),
                        Some(domains),
                        "{}",
                        request.tools[tool]
                    );
                    assert!(
                        banner.contains(&format!("{} {tool} (only docs.rs)", search.provider)),
                        "{banner}"
                    );
                }
                None => {
                    assert!(
                        banner.contains(&format!(
                            "{} {tool} (every domain: {} takes no search filter)",
                            search.provider, search.provider
                        )),
                        "{banner}"
                    );
                }
            }
            // The fetch allowlist reaches neither the declaration nor the banner (I73).
            assert!(!request.tools[tool].to_string().contains("internal.example"));
            assert!(!banner.contains("internal.example"), "{banner}");
            assert_eq!(f.records("harness.web.provider_enabled").len(), 1, "once per run");
            assert_eq!(
                f.harness.agent().info().web_search,
                WebSearchPath::Provider {
                    provider: search.provider.into(),
                    tool: tool.into(),
                    filter: Some(only_docs_rs()),
                    filter_applied: forwarded.is_some(),
                }
            );
        }
    }
}

/// spec 3.1 §7.32, I73: the fetch allowlist is never forwarded as a search filter.
#[tokio::test]
async fn the_fetch_allowlist_is_never_a_search_filter() {
    for (search, tool) in [(anthropic(), "web_search"), (openai(), "openai_web_search")] {
        let f = common::fixture_on(model(Some(search), vec![done()]), |b| {
            b.web(web(&["docs.rs", "*.rust-lang.org"]))
        })
        .await;
        f.send("hello").await;
        let request = f.model.requests().into_iter().next().unwrap();
        let declaration = request.tools[tool].to_string();
        assert!(!declaration.contains("docs.rs"), "{declaration}");
        assert!(!declaration.contains("rust-lang.org"), "{declaration}");
        let banner = banner(&request).unwrap();
        assert!(banner.contains(&format!("{} {tool} (every domain)", search.provider)), "{banner}");
        let record = &f.records("harness.web.provider_enabled")[0].ext;
        assert!(record.get("searchDomains").is_none_or(Value::is_null), "{record:?}");
        assert_eq!(
            f.harness.agent().info().web_search.to_string(),
            format!("{} {tool}, run by the provider: every domain", search.provider)
        );
    }
}

#[tokio::test]
async fn a_blocked_filter_reaches_anthropic_and_is_named_as_not_applied_elsewhere() {
    let blocked = SearchDomains::Blocked(vec!["pinterest.com".into()]);
    let f = common::fixture_on(model(Some(anthropic()), vec![done()]), |b| {
        b.web(web_with_filter(blocked.clone()))
    })
    .await;
    f.send("hello").await;
    let request = f.model.requests().into_iter().next().unwrap();
    assert_eq!(
        request.tools["web_search"].pointer("/x-adk-anthropic-tool/blocked_domains"),
        Some(&json!(["pinterest.com"]))
    );
    assert_eq!(
        f.records("harness.web.provider_enabled")[0].ext["searchDomains"],
        json!({ "blocked": ["pinterest.com"] })
    );

    // OpenAI's search takes an allowed list only.
    let f = common::fixture_on(model(Some(openai()), vec![done()]), |b| {
        b.web(web_with_filter(blocked.clone()))
    })
    .await;
    f.send("hello").await;
    let request = f.model.requests().into_iter().next().unwrap();
    assert!(!request.tools["openai_web_search"].to_string().contains("pinterest"));
    let banner = banner(&request).unwrap();
    assert!(
        banner.contains("openai openai_web_search (every domain: openai takes no search filter)")
    );
    assert!(
        f.harness.agent().info().web_search.to_string().contains("search_domains is not applied")
    );
}

#[tokio::test]
async fn a_filter_that_allows_no_domain_declares_no_search() {
    let f = common::fixture_on(model(Some(anthropic()), vec![done()]), |b| {
        b.web(web_with_filter(SearchDomains::Allowed(Vec::new())))
    })
    .await;
    f.send("hello").await;
    assert!(!f.model.requests()[0].tools.contains_key("web_search"));
    match &f.harness.agent().info().web_search {
        WebSearchPath::None { reason } => assert!(reason.contains("allows no domain"), "{reason}"),
        other => panic!("expected no search, got {other:?}"),
    }
}

/// The governed `web_search` applies the filter to its results.
#[tokio::test]
async fn the_governed_search_drops_results_outside_the_filter() {
    let f = common::fixture_on(
        model(
            Some(gemini()),
            vec![
                ScriptedResponse::function_call(
                    "web_search",
                    json!({ "query": "adk core", "site": "docs.rs" }),
                ),
                done(),
            ],
        ),
        |b| {
            // docs.rs is fetched without approval, and its search results are filtered out.
            let filter = SearchDomains::Blocked(vec!["docs.rs".into()]);
            b.web(WebConfig { search_domains: Some(filter), ..web(&["docs.rs"]) })
                .search_provider(Arc::new(FixedSearch))
        },
    )
    .await;
    f.send("search").await;
    assert_eq!(
        f.harness.agent().info().web_search.to_string(),
        "web_search through fixed (governed): every domain except docs.rs"
    );
    let record = &searched(&f)[0].ext;
    assert_eq!((record["results"].clone(), record["filtered"].clone()), (json!(0), json!(1)));
}

#[tokio::test]
async fn a_model_whose_client_serves_no_search_declares_none_and_says_why() {
    let f = common::fixture_on(model(None, vec![done()]), |b| b.web(web(&[]))).await;
    f.send("hello").await;
    let request = f.model.requests().into_iter().next().unwrap();
    for tool in ["google_search", "web_search", "openai_web_search", "deepseek_web_search"] {
        assert!(!request.tools.contains_key(tool), "{:?}", request.tools.keys());
    }
    assert!(banner(&request).is_none(), "no banner without a live provider tool");
    match &f.harness.agent().info().web_search {
        WebSearchPath::None { reason } => {
            assert!(reason.contains("serves no provider-native search"), "{reason}");
            assert!(reason.contains("search provider"), "{reason}");
        }
        other => panic!("expected no search, got {other:?}"),
    }
}

#[tokio::test]
async fn a_configured_search_provider_replaces_the_provider_search() {
    let f = common::fixture_on(model(Some(gemini()), vec![done()]), |b| {
        b.web(web(&["docs.rs"])).search_provider(Arc::new(FixedSearch))
    })
    .await;
    f.send("hello").await;
    let request = f.model.requests().into_iter().next().unwrap();
    assert!(request.tools.contains_key("web_search"), "the governed web_search");
    assert!(!request.tools.contains_key("google_search"), "{:?}", request.tools.keys());
    assert!(f.records("harness.web.provider_enabled").is_empty());
    assert_eq!(
        f.harness.agent().info().web_search,
        WebSearchPath::Governed { provider: "fixed".into(), filter: None }
    );
}

#[tokio::test]
async fn off_and_a_harness_without_web_declare_no_provider_search() {
    let f = common::fixture_on(model(Some(anthropic()), vec![done()]), |b| {
        b.web(web(&[])).provider_web_tools(ProviderWebTools::off())
    })
    .await;
    f.send("hello").await;
    assert!(!f.model.requests()[0].tools.contains_key("web_search"));
    assert!(matches!(f.harness.agent().info().web_search, WebSearchPath::None { .. }));

    let f = common::fixture_on(model(Some(anthropic()), vec![done()]), |b| b).await;
    f.send("hello").await;
    assert!(!f.model.requests()[0].tools.contains_key("web_search"));
    assert_eq!(
        f.harness.agent().info().web_search,
        WebSearchPath::None { reason: "web is off".into() }
    );

    // Explicit control still works without `web`: the operator asked for it.
    let f = common::fixture_on(model(Some(anthropic()), vec![done()]), |b| {
        b.provider_web_tools(ProviderWebTools::provider_default())
    })
    .await;
    f.send("hello").await;
    assert!(f.model.requests()[0].tools.contains_key("web_search"));
}

fn searched(f: &common::Fixture) -> Vec<adk_core::audit_log::AuditRecordDraft> {
    f.records("harness.web.searched")
}

#[tokio::test]
async fn what_anthropic_searched_is_recorded_after_the_response() {
    let response = LlmResponse::new(Content {
        role: "model".into(),
        parts: vec![
            Part::ServerToolCall {
                server_tool_call: json!({
                    "id": "srvtoolu_1", "name": "web_search",
                    "input": { "query": "rust 1.95 release token=AKIAIOSFODNN7EXAMPLE" }
                }),
            },
            Part::ServerToolResponse {
                server_tool_response: json!({
                    "tool_use_id": "srvtoolu_1",
                    "content": [
                        { "type": "web_search_result", "url": "https://blog.rust-lang.org/2026/rust-1.95", "title": "Rust 1.95", "encrypted_content": "e1" },
                        { "type": "web_search_result", "url": "https://doc.rust-lang.org/releases", "title": "Releases", "encrypted_content": "e2" }
                    ]
                }),
            },
            Part::Text { text: "Rust 1.95 is the latest stable release.".into() },
        ],
    });
    let f = common::fixture_on(
        model(Some(anthropic()), vec![ScriptedResponse::response(response)]),
        |b| b.web(web(&[])),
    )
    .await;
    f.send("what is the latest rust?").await;
    let records = searched(&f);
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0].ext;
    assert_eq!(record["source"], "provider");
    assert_eq!(record["provider"], "anthropic");
    assert_eq!(record["tool"], "web_search");
    let query = record["queries"][0].as_str().unwrap();
    assert!(query.starts_with("rust 1.95 release") && !query.contains("AKIA"), "{query}");
    assert_eq!(record["queryDigests"].as_array().unwrap().len(), 1);
    assert_eq!(record["results"], 2);
    assert_eq!(record["hosts"], json!(["blog.rust-lang.org", "doc.rust-lang.org"]));
}

#[tokio::test]
async fn what_gemini_grounded_on_is_recorded_after_the_response() {
    let mut response = LlmResponse::new(Content::new("model").with_text("Rust 1.95."));
    response.provider_metadata = Some(json!({
        "webSearchQueries": ["latest stable rust version"],
        "groundingChunks": [
            { "web": { "uri": "https://vertexaisearch.cloud.google.com/grounding-api-redirect/a", "title": "rust-lang.org" } }
        ]
    }));
    let f = common::fixture_on(
        model(Some(gemini()), vec![ScriptedResponse::response(response)]),
        |b| b.web(web(&[])),
    )
    .await;
    f.send("what is the latest rust?").await;
    let records = searched(&f);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].ext["provider"], "gemini");
    assert_eq!(records[0].ext["tool"], "google_search");
    assert_eq!(records[0].ext["queries"], json!(["latest stable rust version"]));
    assert_eq!(records[0].ext["results"], 1);
}

#[tokio::test]
async fn a_response_without_a_search_writes_no_record() {
    let f = common::fixture_on(model(Some(anthropic()), vec![done()]), |b| b.web(web(&[]))).await;
    f.send("hello").await;
    assert!(searched(&f).is_empty());
}

fn refusal(category: adk_core::ErrorCategory) -> adk_core::AdkError {
    adk_core::AdkError::new(
        adk_core::ErrorComponent::Model,
        category,
        "model.fixture.refused",
        "You exceeded your current quota",
    )
}

#[tokio::test]
async fn a_provider_that_refuses_its_built_in_search_gets_the_request_again_without_it() {
    for first in [
        // Refused when the call starts, and as the stream's first item.
        ScriptedResponse::error(refusal(adk_core::ErrorCategory::RateLimited)),
        ScriptedResponse::stream(Vec::<LlmResponse>::new())
            .then_error(refusal(adk_core::ErrorCategory::InvalidInput)),
    ] {
        let f = common::fixture_on(model(Some(gemini()), vec![first, done(), done()]), |b| {
            b.web(web(&[]))
        })
        .await;
        let events = f.send("hello").await;
        let text: String = events
            .iter()
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| &content.parts)
            .filter_map(|part| part.text())
            .collect();
        assert!(text.contains("done"), "the turn completes: {text}");
        let requests = f.model.requests();
        assert!(requests[0].tools.contains_key("google_search"));
        assert!(!requests[1].tools.contains_key("google_search"), "retried without it");
        assert!(banner(&requests[1]).is_none(), "and without the banner");
        f.send("again").await;
        let requests = f.model.requests();
        assert!(!requests[2].tools.contains_key("google_search"), "it stays off");
        assert!(banner(&requests[2]).is_none());
    }
}

#[tokio::test]
async fn any_other_model_error_is_not_retried() {
    let f = common::fixture_on(
        model(
            Some(gemini()),
            vec![ScriptedResponse::error(refusal(adk_core::ErrorCategory::Internal)), done()],
        ),
        |b| b.web(web(&[])),
    )
    .await;
    let stream = f.harness.send(&f.user, &f.session, "hello").await.unwrap();
    let results: Vec<_> = futures::StreamExt::collect::<Vec<_>>(stream).await;
    assert!(results.iter().any(Result::is_err), "the model error ends the run");
    assert_eq!(f.model.requests().len(), 1, "no second request");
}

/// A loopback server answering one request with `body`, keeping the request it read.
async fn one_shot(body: &'static str) -> (String, Arc<Mutex<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(String::new()));
    let keep = seen.clone();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else { return };
        let mut buf = vec![0u8; 16 * 1024];
        let mut read = 0;
        loop {
            let Ok(n) = socket.read(&mut buf[read..]).await else { return };
            read += n;
            let text = String::from_utf8_lossy(&buf[..read]).into_owned();
            if let Some(head_end) = text.find("\r\n\r\n") {
                let length = text[..head_end]
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if read >= head_end + 4 + length || n == 0 {
                    *keep.lock().unwrap() = text;
                    break;
                }
            }
            if n == 0 {
                return;
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.shutdown().await;
    });
    (format!("http://127.0.0.1:{}", addr.port()), seen)
}

#[tokio::test]
async fn searxng_queries_the_json_api_with_site_and_maps_hits() {
    let (base, seen) = one_shot(
        r#"{"results":[{"title":"adk-core","url":"https://docs.rs/adk-core","content":"Core traits."},{"title":"no url"}]}"#,
    )
    .await;
    let hits =
        SearxngSearch::new(&base).unwrap().search("adk core", 5, Some("docs.rs")).await.unwrap();
    assert_eq!(
        hits,
        vec![SearchHit {
            title: "adk-core".into(),
            url: "https://docs.rs/adk-core".into(),
            snippet: "Core traits.".into()
        }]
    );
    let request = seen.lock().unwrap().clone();
    assert!(request.starts_with("GET /search?q=adk+core+site%3Adocs.rs&format=json "), "{request}");
}

#[tokio::test]
async fn deepseek_search_posts_the_query_with_the_key_and_maps_summaries() {
    let (base, seen) = one_shot(
        r#"{"results":[{"rerank_score":2.1,"title":"Rust 1.95","url":"https://blog.rust-lang.org/x","snippet":"short","summary":"Rust 1.95 was released.","content":""}]}"#,
    )
    .await;
    let search =
        DeepSeekSearch::new("sk-fixture").unwrap().with_endpoint(format!("{base}/v1/tools/search"));
    let hits = search.search("latest rust", 3, None).await.unwrap();
    assert_eq!(
        hits,
        vec![SearchHit {
            title: "Rust 1.95".into(),
            url: "https://blog.rust-lang.org/x".into(),
            snippet: "Rust 1.95 was released.".into()
        }]
    );
    let request = seen.lock().unwrap().clone();
    assert!(request.starts_with("POST /v1/tools/search "), "{request}");
    assert!(request.to_ascii_lowercase().contains("authorization: bearer sk-fixture"), "{request}");
    let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body, json!({ "query": "latest rust", "count": 3 }));
    assert!(DeepSeekSearch::new(" ").is_err());
}
