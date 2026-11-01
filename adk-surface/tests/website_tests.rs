//! The website's views over HTTP (spec 3.5 §8.2, §7.17, §7.18, §7.21, §7.24, §7.8.2): the
//! setup code gates onboarding (A-I20); onboarding runs from an online presence to a signed
//! release; a public body's launch and edits wait for two people (A-I23); the site view's
//! decisions are an owner's and promotions are earned (A-I13); the weekly review reaches the
//! owner's channel; the web editor drafts, previews, schedules, corrects and withdraws, and
//! refuses an executable upload (A-I1).

#![cfg(feature = "website")]

#[path = "../examples/scripted_website/site.rs"]
mod site;

use std::path::Path;

use adk_core::RequestContext;
use awp_types::SiteKind;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The desk's routes as `adk-rust up` mounts them, with the caller named by `x-person` (the
/// owner when absent), as the server's extractor would authenticate them.
fn app(desk: &adk_website::ui::WebsiteDesk) -> axum::Router {
    axum::Router::new()
        .nest("/api", adk_surface::web::website::router(desk.clone()))
        .layer(axum::middleware::from_fn(
            |mut request: Request<Body>, next: axum::middleware::Next| async move {
                let person = request
                    .headers()
                    .get("x-person")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or(site::OWNER)
                    .to_string();
                if person != "nobody" {
                    request.extensions_mut().insert(Some(RequestContext {
                        user_id: person,
                        scopes: Vec::new(),
                        metadata: Default::default(),
                    }));
                }
                next.run(request).await
            },
        ))
        .merge(adk_surface::web::website::setup_router(desk.clone()))
}

struct Client {
    app: axum::Router,
    token: Option<String>,
    person: Option<String>,
}

impl Client {
    fn new(desk: &adk_website::ui::WebsiteDesk) -> Self {
        Self { app: app(desk), token: None, person: None }
    }

    fn as_person(&self, person: &str) -> Self {
        Self { app: self.app.clone(), token: self.token.clone(), person: Some(person.into()) }
    }

    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(uri);
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        if let Some(token) = &self.token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        if let Some(person) = &self.person {
            request = request.header("x-person", person);
        }
        let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));
        let response = self.app.clone().oneshot(request.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn get(&self, uri: &str) -> Value {
        let (status, body) = self.call("GET", uri, None).await;
        assert_eq!(status, StatusCode::OK, "GET {uri}: {body}");
        body
    }

    async fn post(&self, uri: &str, body: Value) -> Value {
        let (status, answer) = self.call("POST", uri, Some(body.clone())).await;
        assert_eq!(status, StatusCode::OK, "POST {uri} {body}: {answer}");
        answer
    }

    async fn onboard(&self, command: Value) -> Value {
        self.post("/api/surface/website/onboarding", command).await
    }

    async fn refused(&self, uri: &str, body: Value) -> (StatusCode, String) {
        let (status, answer) = self.call("POST", uri, Some(body)).await;
        assert!(!status.is_success(), "{uri} should refuse: {answer}");
        (status, answer["error"]["code"].as_str().unwrap_or_default().to_string())
    }
}

async fn claim(client: &mut Client, code: &str) {
    let claimed = client
        .post(
            "/api/surface/website/setup/claim",
            json!({ "code": code, "device": "Amina's laptop" }),
        )
        .await;
    client.token = Some(claimed["token"].as_str().unwrap().to_string());
}

fn workspace() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn titles(list: &Value, key: &str) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|v| v[key].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn a_fresh_site_takes_no_onboarding_input_before_its_setup_code() {
    let dir = workspace();
    let site = site::fresh(dir.path(), SiteKind::Business).await;
    let mut client = Client::new(&site.desk);

    let setup = client.get("/api/surface/website/setup").await;
    assert_eq!((setup["needed"].as_bool(), setup["paired"].as_bool()), (Some(true), Some(false)));
    let (status, _) = client.call("GET", "/api/surface/website/onboarding", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "A-I20: nothing before the code");
    let start = json!({ "type": "start", "from": "nothing", "name": "Juniper Bakery" });
    assert_eq!(
        client.refused("/api/surface/website/onboarding", start.clone()).await,
        (StatusCode::UNAUTHORIZED, "website.setup.unpaired".into())
    );
    let (status, _) = client
        .call("POST", "/api/surface/website/setup/claim", Some(json!({ "code": "WRONG-CODE" })))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    claim(&mut client, site.setup_code.as_deref().unwrap()).await;
    let stranger = Client { app: client.app.clone(), token: None, person: None };
    assert_eq!(
        stranger
            .refused(
                "/api/surface/website/setup/claim",
                json!({ "code": site.setup_code.clone().unwrap() })
            )
            .await
            .0,
        StatusCode::FORBIDDEN,
        "one code pairs one device"
    );
    let impostor =
        Client { app: client.app.clone(), token: Some("adkd_guess_secret".into()), person: None };
    assert_eq!(
        impostor.refused("/api/surface/website/onboarding", start.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    let started = client.onboard(start).await;
    assert_eq!(started["view"]["current"], "discover");
    let home = client.get("/api/surface/website").await;
    assert_eq!(home["needsSetup"], false);
}

#[tokio::test]
async fn onboarding_runs_from_an_online_presence_to_a_signed_release() {
    let dir = workspace();
    let site = site::fresh(dir.path(), SiteKind::Business).await;
    let mut client = Client::new(&site.desk);
    claim(&mut client, site.setup_code.as_deref().unwrap()).await;

    // 1–2: the presence is read; every fact is a guess with its source (A-I18, S2).
    let started = client
        .onboard(json!({ "type": "start", "from": "presence", "profiles": ["https://maps.example/place/juniper-bakery-nakuru"], "phone": "+254 700 123 456" }))
        .await;
    let discovery = &started["view"]["discovery"];
    assert_eq!(discovery["kind"]["proposed"], "business");
    let facts = discovery["facts"].as_array().unwrap();
    assert!(facts.len() >= 4);
    assert!(facts.iter().all(|f| f["status"]["text"] == "A guess"
        && f["source"].as_str().is_some_and(|s| s.starts_with("https://"))));
    assert!(
        titles(&discovery["facts"], "text").iter().any(|t| t == "Opening hours: Mo-Sa 07:00-18:00")
    );
    let mut confirmed = Vec::new();
    for fact in facts.iter().filter(|f| !f["text"].as_str().unwrap().starts_with("Kind of place")) {
        let after =
            client.onboard(json!({ "type": "fact", "id": fact["id"], "answer": "right" })).await;
        confirmed.push(fact["text"].clone());
        // Each confirmed fact stays listed in the words the owner read, whatever its key's case.
        let listed = after["view"]["discovery"]["facts"].as_array().unwrap();
        assert_eq!(listed.len(), facts.len());
        for text in &confirmed {
            assert!(
                listed.iter().any(|f| &f["text"] == text && f["status"]["text"] == "You said this"),
                "{text} is listed as confirmed"
            );
        }
    }
    assert!(confirmed.iter().any(|t| t.as_str().is_some_and(|t| t.starts_with("Address: "))));
    let after_kind = client.onboard(json!({ "type": "kind", "kind": "business" })).await;
    assert_eq!(after_kind["view"]["current"], "interview", "no legacy site: nothing to protect");

    // 4: each answer rebuilds the live preview from Known facts only.
    let first = client
        .onboard(json!({ "type": "answer", "form": "about", "values": { "mission": "Fresh rye and sourdough for Nakuru." } }))
        .await;
    let preview = &first["view"]["interview"]["preview"];
    assert!(preview["about"].as_str().unwrap().contains("Fresh rye and sourdough"), "{preview}");
    // Confirmed facts are quoted in the words the owner read, never with discovery's keys.
    let about = preview["about"].as_str().unwrap();
    assert!(about.contains("Address: ") && !about.contains("address: "), "{about}");
    let facts_before = preview["facts"].as_u64().unwrap();
    let second = client
        .onboard(json!({ "type": "answer", "form": "who", "values": { "who": "Amina Odhiambo and her brother Tom" } }))
        .await;
    assert_eq!(second["view"]["interview"]["preview"]["facts"].as_u64().unwrap(), facts_before + 1);
    let hours = second["view"]["interview"]["forms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == "hours")
        .cloned()
        .unwrap();
    assert_eq!(hours["fromDiscovery"], true, "asks only what discovery could not answer");
    let (_, code) = client
        .refused(
            "/api/surface/website/onboarding",
            json!({ "type": "answer", "form": "contact", "values": { "email": "not an address" } }),
        )
        .await;
    assert_eq!(code, "website.onboarding.email");
    client.onboard(json!({ "type": "answer", "form": "contact", "values": { "email": "hello@juniper.example" } })).await;
    client.onboard(json!({ "type": "answer", "form": "alarms", "values": { "urgent": "sms", "soon": "whatsapp" } })).await;
    let drafted = client.onboard(json!({ "type": "finishInterview" })).await;

    // 5: one card per section; no target without a baseline; read before signing.
    let charter = &drafted["view"]["charter"];
    assert_eq!(charter["sections"].as_array().unwrap().len(), 11);
    let goals =
        charter["sections"].as_array().unwrap().iter().find(|s| s["id"] == "goals").unwrap();
    assert!(
        titles(&goals["lines"], "value")
            .iter()
            .filter(|v| v.starts_with("From "))
            .all(|v| v.contains("no target")),
        "A-I14"
    );
    let sign = json!({ "type": "charter", "command": { "type": "sign", "role": "owner" } });
    assert_eq!(
        client.refused("/api/surface/website/onboarding", sign.clone()).await.1,
        "website.charter.unread"
    );
    for section in [
        "purpose",
        "goals",
        "guardrails",
        "authority",
        "thresholds",
        "budget",
        "voice",
        "never",
        "reporting",
        "people",
        "public",
    ] {
        client
            .onboard(
                json!({ "type": "charter", "command": { "type": "review", "section": section } }),
            )
            .await;
    }
    client
        .onboard(json!({ "type": "charter", "command": { "type": "edit", "section": "budget", "values": { "hosting": "1500", "models": "3000", "domains": "1500" } } }))
        .await;
    let signed = client.onboard(sign).await;
    assert_eq!(signed["view"]["charter"]["committed"]["version"], 1);
    assert_eq!(signed["view"]["current"], "build");

    // 6–7: a design direction builds release 1; the launch check passes page by page.
    let built = client.onboard(json!({ "type": "design", "design": "harbour" })).await;
    assert!(built["view"]["design"]["held"].as_array().unwrap().is_empty(), "{}", built["note"]);
    let gated = client.onboard(json!({ "type": "runGate" })).await;
    let gate = &gated["view"]["gate"];
    assert_eq!(gate["passed"], true, "{gate}");
    let pages = gate["pages"].as_array().unwrap();
    assert_eq!(pages[0]["path"], "/index.html");
    assert!(pages.iter().all(|p| p["change"]["text"] == "New page"));
    assert!(
        pages.iter().any(|p| p["markdown"].as_str().is_some_and(|m| m.contains("Amina Odhiambo")))
    );

    // 8: connections say the notice first and never take a password (A-I19); going live is
    // the owner's decision (A-I21).
    let view = &gated["view"];
    assert!(
        view["connections"]["notice"].as_str().unwrap().contains("never ask you for a password")
    );
    let connect = client.onboard(json!({ "type": "connect", "connection": "telegram" })).await;
    assert_eq!(connect["next"]["type"], "code");
    assert!(view["deploy"]["canGoLive"].as_bool().unwrap());
    let live = client.onboard(json!({ "type": "goLive" })).await;
    assert_eq!(live["view"]["current"], "done", "{}", live["note"]);
    assert!(live["view"]["run"]["phases"].as_array().unwrap().len() == 3);
    let runtime = site.desk.runtime().await;
    assert!(
        runtime.host().fetch("/.well-known/awp/log/checkpoint").await.unwrap().is_some(),
        "release 1 is signed and served"
    );
    let actions: Vec<String> = site.audit.records().into_iter().map(|r| r.action).collect();
    for action in
        ["website.onboarding.started", "website.charter.committed", "website.onboarding.completed"]
    {
        assert!(actions.iter().any(|a| a == action), "{action} in {actions:?}");
    }
}

#[tokio::test]
async fn a_public_body_launches_and_publishes_only_with_two_people() {
    let dir = workspace();
    let site = site::fresh(dir.path(), SiteKind::Government).await;
    let mut client = Client::new(&site.desk);
    claim(&mut client, site.setup_code.as_deref().unwrap()).await;
    client.onboard(json!({ "type": "start", "from": "nothing", "name": "Nakuru County" })).await;
    client.onboard(json!({ "type": "kind", "kind": "government" })).await;
    for (form, values) in [
        ("about", json!({ "mission": "County services for Nakuru, online." })),
        ("who", json!({ "who": "The County Communications Office" })),
        ("contact", json!({ "email": "info@juniper.example" })),
        ("hours", json!({ "hours": "Monday to Friday, 08:00 to 17:00" })),
        ("alarms", json!({ "urgent": "sms" })),
    ] {
        client.onboard(json!({ "type": "answer", "form": form, "values": values })).await;
    }
    client.onboard(json!({ "type": "finishInterview" })).await;
    for section in [
        "purpose",
        "goals",
        "guardrails",
        "authority",
        "thresholds",
        "budget",
        "voice",
        "never",
        "reporting",
        "people",
        "public",
    ] {
        client
            .onboard(
                json!({ "type": "charter", "command": { "type": "review", "section": section } }),
            )
            .await;
    }
    let (_, code) = client
        .refused("/api/surface/website/onboarding", json!({ "type": "charter", "command": { "type": "edit", "section": "people", "values": { "person": "urn:adk:agent:juniper/steward", "role": "publisher" } } }))
        .await;
    assert_eq!(code, "website.charter.agent_role", "no agent holds a role");
    for (person, role) in [("baraka", "publisher"), ("chebet", "editor")] {
        client
            .onboard(json!({ "type": "charter", "command": { "type": "edit", "section": "people", "values": { "person": person, "role": role } } }))
            .await;
    }
    let signed = client
        .onboard(json!({ "type": "charter", "command": { "type": "sign", "role": "owner" } }))
        .await;
    assert_eq!(
        signed["view"]["charter"]["committed"]["version"], 1,
        "{}",
        signed["view"]["charter"]["problems"]
    );
    client.onboard(json!({ "type": "design", "design": "harbour" })).await;
    let gated = client.onboard(json!({ "type": "runGate" })).await;
    assert_eq!(gated["view"]["gate"]["passed"], true, "{}", gated["view"]["gate"]);

    // The launch waits for an editor and a publisher, each a different person (A-I23).
    let waiting = client.onboard(json!({ "type": "goLive" })).await;
    let approval = &waiting["view"]["deploy"]["waiting"];
    assert_eq!(titles(&approval["lines"], "role"), ["editor", "publisher"], "{approval}");
    client.onboard(json!({ "type": "approve", "role": "editor" })).await;
    let (_, code) = client
        .refused(
            "/api/surface/website/onboarding",
            json!({ "type": "approve", "role": "publisher" }),
        )
        .await;
    assert_eq!(code, "website.editor.not_an_approver", "the owner is not a publisher");
    // Baraka approves from his own account: a role holder's decision, not onboarding input.
    let baraka = Client { app: client.app.clone(), token: None, person: Some("baraka".into()) };
    let live = baraka.onboard(json!({ "type": "approve", "role": "publisher" })).await;
    assert_eq!(live["view"]["current"], "done", "{}", live["note"]);

    // An editor's notice publishes only after a different editor and a publisher approve.
    let editor = client.as_person(site::OWNER);
    let drafted = editor
        .post("/api/surface/website/editor", json!({ "type": "new", "kind": "notice" }))
        .await;
    let id = drafted["draft"]["id"].as_str().unwrap().to_string();
    editor
        .post(
            "/api/surface/website/editor",
            json!({ "type": "save", "draft": id, "slug": "water-interruption", "language": "en", "fields": { "title": "Water interruption on Monday" }, "body": "Supply stops from 08:00 to 14:00 in the town centre." }),
        )
        .await;
    let published =
        editor.post("/api/surface/website/editor", json!({ "type": "publish", "draft": id })).await;
    assert_eq!(
        published["draft"]["status"]["text"], "Waiting for approval",
        "{}",
        published["note"]
    );
    assert_eq!(
        editor
            .refused(
                "/api/surface/website/editor",
                json!({ "type": "approve", "draft": id, "role": "editor" })
            )
            .await
            .1,
        "website.editor.own_change",
        "the proposer cannot approve"
    );
    client
        .as_person("chebet")
        .post(
            "/api/surface/website/editor",
            json!({ "type": "approve", "draft": id, "role": "editor" }),
        )
        .await;
    // Baraka approves the same request on another surface, such as the kernel's inbox or a
    // chat card: the release's request is the one record every surface decides (#1280).
    let runtime = site.desk.runtime().await;
    let approvals = runtime.approvals();
    let waiting = approvals
        .list(adk_harness::website::SITE_RELEASE_TOOL, &[adk_core::ApprovalState::Pending])
        .await
        .unwrap();
    let [request] = waiting.as_slice() else { panic!("one release waits: {waiting:?}") };
    let view = editor.get(&format!("/api/surface/website/editor/drafts/{id}")).await;
    assert_eq!(
        view["approval"]["lines"][0]["signedBy"], "chebet",
        "the desk shows the request's votes: {view}"
    );
    approvals
        .approve(&request.id, request.version, &adk_core::principal::SubjectRef::human("baraka"))
        .await
        .unwrap();
    let done =
        editor.post("/api/surface/website/editor", json!({ "type": "publish", "draft": id })).await;
    assert!(done["note"].as_str().unwrap().starts_with("Live in release 2"), "{}", done["note"]);
}

#[tokio::test]
async fn the_site_view_shows_the_site_and_takes_only_an_owners_earned_decisions() {
    let dir = workspace();
    let site = site::live(dir.path()).await;
    let client = Client::new(&site.desk);
    let view = client.get("/api/surface/website/site").await;
    assert_eq!(view["status"]["state"]["text"], "Up");
    assert_eq!(
        titles(&view["releases"], "source")[..2],
        ["A correction".to_string(), "An edit in the web editor".to_string()]
    );
    assert!(
        view["releases"][0]["pages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["path"] == "/posts/rye-week/index.html")
    );
    let leads =
        view["kpis"].as_array().unwrap().iter().find(|k| k["id"] == "qualified_leads").unwrap();
    assert_eq!(
        (leads["trend"]["text"].as_str(), leads["target"].as_str()),
        (Some("Better"), Some("4"))
    );
    let response =
        view["kpis"].as_array().unwrap().iter().find(|k| k["id"] == "response_time").unwrap();
    assert_eq!(response["trend"]["text"], "Better", "fewer hours is better");
    let job = |view: &Value, id: &str| {
        view["jobs"].as_array().unwrap().iter().find(|j| j["id"] == id).cloned().unwrap()
    };
    assert_eq!(
        (
            job(&view, "freshness")["offered"].as_bool(),
            job(&view, "freshness")["canPromote"].as_bool()
        ),
        (Some(true), Some(true))
    );
    assert_eq!(job(&view, "seo")["level"], "Just look", "two rejections lowered it");
    assert_eq!(view["signals"]["open"], 3);
    assert_eq!(view["wallet"]["held"][0]["why"], "Over the KES 1,500.00 limit for one payment");
    assert_eq!(
        view["held"]["items"][0]["reason"],
        "The price moves more than the 10% your price rule allows"
    );
    assert!(view["held"]["deliveries"][0]["breaks"][0].as_str().unwrap().contains("at most 20%"));

    let visitor = client.as_person("visitor");
    let (status, _) = visitor.call("GET", "/api/surface/website/site", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "no role, no view");
    assert_eq!(
        visitor
            .refused("/api/surface/website/site", json!({ "type": "promote", "job": "freshness" }))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .refused("/api/surface/website/site", json!({ "type": "promote", "job": "seo" }))
            .await
            .1,
        "website.site.not_earned",
        "autonomy is earned"
    );
    let promoted = client
        .post("/api/surface/website/site", json!({ "type": "promote", "job": "freshness" }))
        .await;
    assert_eq!(job(&promoted["view"], "freshness")["level"], "Ask first");
    let lowered = client
        .post("/api/surface/website/site", json!({ "type": "lower", "job": "weekly-review", "reason": "I read each review first" }))
        .await;
    assert_eq!(job(&lowered["view"], "weekly-review")["level"], "Just look");
    let discarded = client
        .post(
            "/api/surface/website/site",
            json!({ "type": "item", "id": "products/sourdough-loaf/en", "decision": "discard" }),
        )
        .await;
    assert!(discarded["view"]["held"]["items"].as_array().unwrap().is_empty());
    let whole = client
        .post(
            "/api/surface/website/site",
            json!({ "type": "delivery", "delivery": 1, "decision": "discard" }),
        )
        .await;
    assert_eq!(whole["note"], "The whole delivery is discarded.");
}

#[tokio::test]
async fn the_weekly_review_is_shown_and_its_summary_reaches_the_owners_channel() {
    let dir = workspace();
    let site = site::live(dir.path()).await;
    let client = Client::new(&site.desk);
    let page = client.get("/api/surface/website/review").await;
    let review = &page["current"];
    assert_eq!(page["channel"], "whatsapp");
    assert!(review["title"].as_str().unwrap().starts_with("Week of "));
    assert_eq!(review["promotions"][0]["job"], "freshness");
    assert!(
        review["money"]["spend"].as_str().unwrap().starts_with("KES 1,800"),
        "{}",
        review["money"]
    );
    assert_eq!(review["questions"].as_array().unwrap().len(), 2);
    let sent_before = site.phone.sent.lock().unwrap().len();
    let sent = client
        .post("/api/surface/website/review", json!({ "type": "send", "week": review["week"] }))
        .await;
    assert!(sent["current"]["sent"].as_str().unwrap().starts_with("Sent to WhatsApp"));
    let messages = site.phone.sent.lock().unwrap().clone();
    assert_eq!(messages.len(), sent_before + 1);
    assert_eq!(messages.last().unwrap(), review["summary"].as_str().unwrap());
    assert!(site.audit.records().iter().any(|r| r.action == "website.review.sent"));
    let composed = client.post("/api/surface/website/review", json!({ "type": "compose" })).await;
    assert!(!composed["reviews"].as_array().unwrap().is_empty());
    assert_eq!(
        client
            .as_person("visitor")
            .refused("/api/surface/website/review", json!({ "type": "compose" }))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

fn png() -> String {
    use base64::Engine as _;
    let (width, height) = (8u32, 8u32);
    let raw: Vec<u8> = (0..height)
        .flat_map(|_| std::iter::once(0u8).chain([200u8, 120, 40].repeat(width as usize)))
        .collect();
    let chunk = |kind: &[u8], data: &[u8]| {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc_input = kind.to_vec();
        crc_input.extend_from_slice(data);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
        out
    };
    let mut header = width.to_be_bytes().to_vec();
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(chunk(b"IHDR", &header));
    png.extend(chunk(b"IDAT", &zlib_stored(&raw)));
    png.extend(chunk(b"IEND", &[]));
    base64::engine::general_purpose::STANDARD.encode(png)
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A zlib stream of one stored (uncompressed) block.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01, 0x01];
    let len = data.len() as u16;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(data);
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

#[tokio::test]
async fn the_editor_drafts_previews_schedules_corrects_and_withdraws() {
    let dir = workspace();
    let site = site::live(dir.path()).await;
    let client = Client::new(&site.desk);
    let home = client.get("/api/surface/website/editor").await;
    assert_eq!(titles(&home["kinds"], "kind").len(), 9);
    let product =
        home["kinds"].as_array().unwrap().iter().find(|k| k["kind"] == "product").unwrap();
    assert_eq!(
        titles(&product["fields"], "id"),
        ["title", "summary", "price", "currency", "sku", "validUntil"]
    );

    let drafted =
        client.post("/api/surface/website/editor", json!({ "type": "new", "kind": "post" })).await;
    let id = drafted["draft"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        client
            .refused("/api/surface/website/editor", json!({ "type": "save", "draft": id, "slug": "Our Oven", "language": "en", "fields": { "title": "Our new oven" } }))
            .await
            .1,
        "website.editor.slug"
    );
    let saved = client
        .post("/api/surface/website/editor", json!({ "type": "save", "draft": id, "slug": "new-oven", "language": "en", "fields": { "title": "Our new oven", "byline": "Amina" }, "body": "It arrived on **Monday**." }))
        .await;
    assert_eq!(saved["note"], "Saved. Every check passed.");
    let refused = client
        .call(
            "POST",
            &format!("/api/surface/website/editor/drafts/{id}/media"),
            Some(json!({ "name": "oven.png", "dataBase64": "f0VMRgIBAQ==" })),
        )
        .await;
    assert_eq!(refused.0, StatusCode::BAD_REQUEST, "A-I1: {}", refused.1);
    let uploaded = client
        .post(
            &format!("/api/surface/website/editor/drafts/{id}/media"),
            json!({ "name": "Oven.png", "dataBase64": png(), "alt": "The new oven" }),
        )
        .await;
    assert_eq!(
        uploaded["media"][0]["altConfirmed"], false,
        "alt text is a draft until a person confirms it"
    );
    let previewed =
        client.post("/api/surface/website/editor", json!({ "type": "preview", "draft": id })).await;
    assert!(previewed["draft"]["checkText"].as_str().unwrap().contains("Accessibility"));
    let path = uploaded["media"][0]["path"].as_str().unwrap().to_string();
    client.post("/api/surface/website/editor", json!({ "type": "confirmAlt", "draft": id, "path": path, "alt": "The new oven, still wrapped" })).await;
    let previewed =
        client.post("/api/surface/website/editor", json!({ "type": "preview", "draft": id })).await;
    let pages = previewed["draft"]["preview"].as_array().unwrap();
    assert_eq!(
        (pages[0]["path"].as_str(), pages[0]["change"]["text"].as_str()),
        (Some("/posts/new-oven/index.html"), Some("New page"))
    );
    let published =
        client.post("/api/surface/website/editor", json!({ "type": "publish", "draft": id })).await;
    assert_eq!(published["note"], "Live in release 5, signed.");

    let corrected = client
        .post("/api/surface/website/editor", json!({ "type": "correct", "id": "posts/new-oven/en", "note": "It arrived on Tuesday.", "body": "It arrived on **Tuesday**." }))
        .await;
    assert_eq!(corrected["note"], "Live in release 6, signed.");
    let withdrawn = client
        .post("/api/surface/website/editor", json!({ "type": "withdraw", "id": "posts/new-oven/en", "notice": "We took this post down." }))
        .await;
    let state = |home: &Value, id: &str| {
        home["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .map(|i| i["state"]["text"].clone())
    };
    assert_eq!(state(&withdrawn["home"], "posts/new-oven/en"), Some(json!("Withdrawn")));

    let event =
        client.post("/api/surface/website/editor", json!({ "type": "new", "kind": "event" })).await;
    let event_id = event["draft"]["id"].as_str().unwrap().to_string();
    let later = (chrono::Utc::now() + chrono::Duration::days(3)).to_rfc3339();
    client
        .post("/api/surface/website/editor", json!({ "type": "save", "draft": event_id, "slug": "bread-class", "language": "en", "fields": { "title": "Sourdough class", "startsAt": later, "location": "The bakery" }, "schedule": { "publishAt": later } }))
        .await;
    let scheduled = client
        .post("/api/surface/website/editor", json!({ "type": "publish", "draft": event_id }))
        .await;
    assert_eq!(scheduled["draft"]["status"]["text"], "Scheduled", "{}", scheduled["note"]);
    assert_eq!(state(&scheduled["home"], "events/bread-class/en"), Some(json!("Scheduled")));
    assert!(Path::new(&site.host_dir).exists());
}
