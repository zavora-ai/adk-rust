//! Outbound Web Bot Auth and one scripted onboarding pass (spec 3.5 §7.3 "Outbound", §7.7,
//! §7.18, A-I18, A-I19, A-I21, A-I23) against a loopback site that verifies every signature:
//! discovery records `Assumed` facts from a signed crawl, the interview records `Known` facts
//! only from the owner's words, every required signer signs the charter, release 1 is built
//! from `Known` facts, gated, launched with the owner's approval, and `awp verify` passes —
//! until a page on the host is replaced.

#![cfg(all(feature = "website", feature = "web-fixture", feature = "graph"))]

use crate::common;
use crate::website_common;

use std::sync::Arc;

use adk_auth::web_bot_auth::Ed25519SigningKey;
use adk_core::principal::SubjectRef;
use adk_harness::web::WebConfig;
use adk_harness::website::{
    CONNECTION_NOTICE, OnboardingStage, ReleaseLogVerifier, SiteSigning, onboarding_plan,
};
use adk_harness::{FactFilter, FactStatus, FactStore, HarnessMode, PlanGate};
use adk_kernel::charter::{CharterStore, RoleApproval};
use adk_model::ScriptedResponse;
use awp_types::Role;
use common::{ScriptedPrompter, collect, responses, user};
use serde_json::{Value, json};
use website_common::server::SiteServer;
use website_common::{OWNER, World};

fn call(tool: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(tool, args)
}

fn response(events: &[adk_core::Event], tool: &str, nth: usize) -> Value {
    responses(events)
        .into_iter()
        .filter(|(name, _)| name == tool)
        .nth(nth)
        .map(|(_, response)| response)
        .unwrap_or_else(|| panic!("no response {nth} of {tool} in {:#?}", responses(events)))
}

fn signing() -> SiteSigning {
    let key = Ed25519SigningKey::from_seed([3; 32]).unwrap();
    SiteSigning::new(Arc::new(key), "https://bakery.example").unwrap()
}

/// `web_fetch` is signed on every hop when the operator opts in, and unsigned otherwise.
#[tokio::test]
async fn web_fetch_is_signed_on_every_redirect_hop_when_opted_in() {
    let signing = signing();
    let server = SiteServer::start(None, Some(&signing)).await;
    let mut world = World::new(true, false).await;
    let web = WebConfig { allowed_domains: vec!["127.0.0.1".into()], ..WebConfig::default() };
    let url = server.url("/redirect");
    let (signed, _) = world.back_office(
        "growth",
        vec![call("web_fetch", json!({ "url": url })), ScriptedResponse::text("read")],
        |b| b.web(web.clone()).web_signer(signing.request_signer()),
    );
    let owner = user(OWNER);
    let session = signed.open_session(&owner, None).await.unwrap();
    let events =
        collect(signed.send(&owner, &session, "read the supplier's page").await.unwrap()).await;
    assert!(response(&events, "web_fetch", 0).to_string().contains("Juniper Bakery"));
    let requester = format!("wba:https://bakery.example#{}", signing.keyid());
    let hits = server.hits();
    assert_eq!(hits.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(), ["/redirect", "/page"]);
    assert!(
        hits.iter().all(|hit| hit.requester.as_deref() == Some(requester.as_str())),
        "{hits:?}"
    );

    let (unsigned, _) = world.back_office(
        "growth",
        vec![
            call("web_fetch", json!({ "url": server.url("/page") })),
            ScriptedResponse::text("read"),
        ],
        |b| b.web(web.clone()),
    );
    let session = unsigned.open_session(&owner, None).await.unwrap();
    collect(unsigned.send(&owner, &session, "read it again").await.unwrap()).await;
    assert_eq!(
        server.hits().last().unwrap().requester,
        None,
        "general fetches sign only when opted in"
    );
}

/// One onboarding pass through the plan's steps on scripted models.
#[tokio::test]
async fn one_scripted_onboarding_pass() {
    let plan = onboarding_plan(&OnboardingStage::ALL);
    adk_harness::compile_plan(&plan, "onboarding").unwrap();
    let gated: Vec<&str> = plan
        .iter()
        .filter(|step| step.gate == PlanGate::Approve)
        .map(|step| step.id.as_str())
        .collect();
    assert_eq!(gated, ["charter_sign", "connect", "deploy"]);
    let connect = plan.iter().find(|step| step.id == "connect").unwrap();
    assert!(
        connect.instruction.contains(CONNECTION_NOTICE),
        "the site says it before the first connection (A-I19)"
    );

    let signing = signing();
    let mut world = World::new_with(false, false, |runtime| {
        runtime.crawler_signer(signing.request_signer()).verifier(Arc::new(
            ReleaseLogVerifier::new("bakery.example", Vec::new())
                .with_signer(signing.request_signer()),
        ))
    })
    .await;
    let server = SiteServer::start(Some(world.host.clone()), Some(&signing)).await;
    let prompter = ScriptedPrompter::new();
    for _ in 0..2 {
        prompter.approve_once();
    }
    let (steward, model) = world.back_office(
        "steward",
        vec![
            // 2 Discover: a signed crawl; what it states stays unconfirmed.
            call("site_discover", json!({ "url": server.url("/page"), "facts": ["The bakery opens at 7am on weekdays", "The bakery was founded in 1990"] })),
            ScriptedResponse::text("I found the existing page."),
            // 4 Interview: one answer in the owner's words, one the owner never said.
            call("interview_record", json!({ "text": "The bakery opens at seven every weekday", "quote": "We open at seven every weekday" })),
            call("interview_record", json!({ "text": "The bakery was founded in 1990", "quote": "we were founded in 1990" })),
            ScriptedResponse::text("Noted."),
        ],
        |b| b.mode(HarnessMode::Ask).prompter(prompter.clone()),
    );
    let owner = user(OWNER);
    let session = steward.open_session(&owner, None).await.unwrap();
    let send = |message: &'static str| {
        let (steward, owner, session) = (&steward, owner.clone(), session.clone());
        async move { collect(steward.send(&owner, &session, message).await.unwrap()).await }
    };

    let discovered = send("Here is my current page.").await;
    let found = response(&discovered, "site_discover", 0);
    assert_eq!(found["recorded"].as_array().unwrap().len(), 2);
    assert!(found["content"].as_str().unwrap().starts_with("<untrusted-content source=\"web\""));
    let requester = format!("wba:https://bakery.example#{}", signing.keyid());
    assert_eq!(
        server.hits()[0].requester.as_deref(),
        Some(requester.as_str()),
        "the crawl is signed"
    );

    let interview = send("We open at seven every weekday and close at six.").await;
    assert_eq!(response(&interview, "interview_record", 0)["status"], "known");
    assert_eq!(response(&interview, "interview_record", 1)["status"], "assumed");
    let facts = world.facts.list(FactFilter::user(&world.site.name, OWNER)).await.unwrap();
    let known: Vec<_> = facts.iter().filter(|fact| fact.status == FactStatus::Known).collect();
    assert_eq!(known.len(), 1, "Known only from the owner's own words (A-I18)");
    assert_eq!(known[0].provenance.quote.as_deref(), Some("We open at seven every weekday"));
    assert_eq!(facts.len(), 3, "the unconfirmed answer is the fact discovery found");
    let assumed = facts.iter().find(|fact| fact.status == FactStatus::Assumed).unwrap().id.clone();

    // 5 Charter: drafted, requested as a decision card, committed when the owner signs.
    let charter = website_common::charter(&world.site, false);
    model.push(call("charter_draft", json!({ "charter": charter })));
    model.push(call("charter_commit_request", json!({ "draft": "charter-1" })));
    model.push(ScriptedResponse::text("Please sign the charter."));
    let drafted = send("Draft the charter.").await;
    let request = response(&drafted, "charter_commit_request", 0);
    assert_eq!(request["committed"], false);
    assert!(world.charters.current(world.site.id).await.unwrap().is_none());
    let signed = world
        .runtime
        .sign_charter(
            request["request"].as_str().unwrap(),
            RoleApproval::new(Role::Owner, SubjectRef::human(OWNER), chrono::Utc::now()),
            "dec-1",
        )
        .await
        .unwrap()
        .expect("the owner is every required signer");
    assert_eq!(signed.version(), 1);
    world.ladder.sync(&signed.charter).await.unwrap();

    // 6 Build from Known facts; a page citing an Assumed fact is held by the gate.
    let hours = json!({
        "kind": "page", "slug": "hours", "language": "en", "job": "launch",
        "fields": { "title": "Opening hours", "summary": "When we are open." },
        "body": "We open at seven every weekday.",
        "sources": [known[0].id],
    });
    let history = json!({
        "kind": "page", "slug": "history", "language": "en", "job": "launch",
        "fields": { "title": "Our history" }, "body": "Founded in 1990.", "sources": [assumed],
    });
    model.push(call("content_draft", hours));
    model.push(call("content_draft", history));
    model.push(call("release_preview", json!({ "draft": "draft-3" })));
    model.push(ScriptedResponse::text("Here is the preview."));
    let built = send("Build the site.").await;
    assert_eq!(response(&built, "content_draft", 0)["gate"]["passed"], true);
    assert_eq!(
        response(&built, "content_draft", 1)["gate"]["passed"],
        false,
        "a claim from an Assumed fact"
    );
    let preview = response(&built, "release_preview", 0);
    assert_eq!(preview["gate"]["passed"], true, "{preview}");

    // 7 Launch gate, 8 launch (held for the owner, A-I21), then `awp verify`.
    model.push(call("launch_gate_run", json!({})));
    model.push(call("site_publish", json!({ "draft": "draft-3" })));
    model.push(call("awp_verify", json!({ "origin": server.url("") })));
    model.push(ScriptedResponse::text("Live and verified."));
    let launched = send("Launch it.").await;
    let gate = response(&launched, "launch_gate_run", 0);
    assert_eq!(gate["passed"], true, "{}", gate["summary"]);
    let live = response(&launched, "site_publish", 0);
    assert_eq!(live["published"], true, "{live}");
    let held: Vec<String> =
        prompter.prompts.lock().unwrap().iter().map(|p| p.request.tool_name.clone()).collect();
    assert_eq!(held, ["charter_commit_request", "site_publish"]);
    let verified = response(&launched, "awp_verify", 0);
    assert_eq!(verified["passed"], false, "no release key is trusted yet: {verified}");

    // With the site's release key trusted, the published site verifies; a replaced page
    // fails it.
    let verifier = ReleaseLogVerifier::new("bakery.example", vec![world.release_key.clone()])
        .with_signer(signing.request_signer());
    use adk_harness::website::SiteVerifier;
    let report = verifier.verify(&server.url("")).await.unwrap();
    assert!(report.passed(), "{report:?}");
    assert_eq!(report.release, Some(1));
    let page = world.dirs[0].path().join("releases/1/hours/index.html");
    assert!(page.exists());
    // Replaced as an intruder replaces a file; `DirectoryHost`'s stored files are read-only.
    let replacement = page.with_extension("tmp");
    std::fs::write(&replacement, "<html>defaced</html>").unwrap();
    std::fs::rename(&replacement, &page).unwrap();
    let report = verifier.verify(&server.url("")).await.unwrap();
    assert!(!report.passed());
    assert!(
        report.checks.iter().any(|c| c.name == "pages" && c.detail.contains("/hours/index.html")),
        "{report:?}"
    );
    assert!(
        server.hits().iter().all(|hit| hit.requester.is_some()),
        "every verifier request is signed"
    );
}

/// A fetcher for `adk-awp`'s verifier that reaches the loopback site for any domain, signing
/// each request as `HttpFetcher` does.
struct LoopbackFetcher {
    base: String,
    signer: Arc<dyn adk_awp::verify::fetch::RequestSigner>,
}

#[async_trait::async_trait]
impl adk_awp::verify::fetch::Fetcher for LoopbackFetcher {
    async fn get(
        &self,
        url: &str,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<adk_awp::verify::fetch::Fetched, adk_awp::verify::fetch::FetchError> {
        let failed =
            |reason: String| adk_awp::verify::fetch::FetchError { url: url.to_string(), reason };
        let mut headers = headers.clone();
        self.signer.sign(url, &mut headers).await.map_err(failed)?;
        let parsed = reqwest::Url::parse(url).map_err(|e| failed(e.to_string()))?;
        let path = parsed.path().to_string();
        // The signature covers the domain's authority, which the loopback site sees as Host.
        let authority =
            parsed.host_str().unwrap_or_default().parse().map_err(|_| failed("host".into()))?;
        headers.insert(reqwest::header::HOST, authority);
        let response = reqwest::Client::new()
            .get(format!("{}{path}", self.base))
            .headers(headers)
            .send()
            .await
            .map_err(|e| failed(e.to_string()))?;
        Ok(adk_awp::verify::fetch::Fetched {
            status: response.status().as_u16(),
            headers: response.headers().clone(),
            body: response.bytes().await.map_err(|e| failed(e.to_string()))?,
        })
    }
}

/// `awp_verify` runs `adk-awp`'s whole verification with signed `GET`s; a site that publishes
/// no contract fails its first step.
#[tokio::test]
async fn awp_verify_runs_adk_awps_verifier_signed() {
    let signing = signing();
    let host_dir = tempfile::tempdir().unwrap();
    let host = Arc::new(adk_site::host::DirectoryHost::new(host_dir.path()));
    let server = SiteServer::start(Some(host), Some(&signing)).await;
    let verifier = adk_awp::verify::Verifier::new(
        Arc::new(LoopbackFetcher { base: server.url(""), signer: signing.verifier_signer() }),
        Arc::new(adk_awp::signing::anchor::FixedTxtResolver::default()),
        Arc::new(adk_awp::verify::pins::InMemoryPinStore::default()),
    );
    let mut world =
        World::new_with(true, false, |runtime| runtime.verifier(Arc::new(verifier))).await;
    let (guardian, _) = world.back_office(
        "guardian",
        vec![
            call("awp_verify", json!({ "origin": "https://bakery.example" })),
            ScriptedResponse::text("checked"),
        ],
        |b| b.mode(HarnessMode::Ask),
    );
    let owner = user(OWNER);
    let session = guardian.open_session(&owner, None).await.unwrap();
    let events = collect(guardian.send(&owner, &session, "verify the site").await.unwrap()).await;
    let report = response(&events, "awp_verify", 0);
    assert_eq!(report["passed"], false, "{report}");
    assert_eq!(report["report"]["checks"][0]["name"], "contract.unavailable", "{report}");
    let requester = format!("wba:https://bakery.example#{}", signing.keyid());
    let hits = server.hits();
    assert!(!hits.is_empty());
    assert!(
        hits.iter().all(|hit| hit.requester.as_deref() == Some(requester.as_str())),
        "{hits:?}"
    );
}
