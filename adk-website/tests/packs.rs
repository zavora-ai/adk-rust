//! Capability packs as signed bundles (spec 3.5 §7.23): the conformance vectors in
//! `tests/fixtures/packs/` — a valid pack, its upgrade, and a bundle for every refusal — run
//! against a project's `packs/` directory; a pack's capabilities on the edge with the handlers
//! it configures; its playbooks in the agents' workspaces; and a served site loading what is
//! installed and refusing a bundle tampered after installation.
//!
//! The vectors are generated here from fixed keys; `ADK_WEBSITE_PACKS_RECORD=1` writes them
//! again.

mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner};
use adk_awp::v2::store::LeadStore;
use adk_website::deploy::{ServeOptions, serve};
use adk_website::edge::handlers::{InMemoryCalendar, Slot};
use adk_website::onboarding::scaffold::{ScaffoldOptions, scaffold};
use adk_website::packs::bundle::{
    BookingOperation, HandlerConfig, PackBundle, PackDriver, PackPlaybook, PackPublisher,
};
use adk_website::packs::installed::{Admission, LoadedPack, PackDir, PackOrigin, PackSet};
use adk_website::packs::trust::{TrustList, TrustedKey};
use adk_website::team::AgentRole;
use adk_website::themes::ThemeId;
use awp_types::{
    CapabilityEffect, CapabilityV2, ConsentRequirement, ConsentScope, ContentKind, LeadRef,
    SiteKind, TrustTier,
};
use axum::http::StatusCode;
use common::Site;
use common::edge::{Shop, edge_shop};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PUBLISHER: &str = "tables.example";
const KID: &str = "tables-2026";

fn publisher_key() -> Ed25519DocumentSigner {
    Ed25519DocumentSigner::from_seed(KID, [0x54; 32]).unwrap()
}

fn trust() -> TrustList {
    TrustList::new().with(
        TrustedKey::new(PUBLISHER, KID, publisher_key().public_key())
            .noted("The conformance vectors' publisher"),
    )
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

fn consent(scope: &str) -> Option<ConsentRequirement> {
    Some(ConsentRequirement {
        scope: ConsentScope::Custom(scope.into()),
        policy_version: "1".into(),
    })
}

/// The vectors' pack: a waiting list served by the forms handler under a scope of its own,
/// the menu by the listings handler, a table's availability and reservation by the bookings
/// handler, a playbook for the Concierge's capture job and a point-of-sale driver.
fn waitlist(version: &str) -> PackBundle {
    let email = json!({ "type": "string", "format": "email", "maxLength": 254 });
    let join = CapabilityV2 {
        consent: consent("waitlist"),
        input_schema: Some(object(
            json!({ "email": email, "party": { "type": "integer", "minimum": 1, "maximum": 12 } }),
            &["email", "party"],
        )),
        ..CapabilityV2::new(
            "join_waitlist",
            "Join tonight's waiting list",
            CapabilityEffect::NonIdempotent,
        )
    };
    let menu = CapabilityV2 {
        input_schema: Some(object(json!({ "query": { "type": "string", "maxLength": 100 } }), &[])),
        ..CapabilityV2::new(
            "list_menu",
            "Tonight's menu at its published prices",
            CapabilityEffect::ReadOnly,
        )
    };
    let availability = CapabilityV2 {
        input_schema: Some(object(
            json!({ "from": { "type": "string", "format": "date" }, "to": { "type": "string", "format": "date" } }),
            &[],
        )),
        ..CapabilityV2::new("table_availability", "Open tables", CapabilityEffect::ReadOnly)
    };
    let reserve = CapabilityV2 {
        consent: consent("contact"),
        input_schema: Some(object(
            json!({ "slot": { "type": "string", "maxLength": 100 }, "name": { "type": "string", "maxLength": 200 }, "email": email }),
            &["slot", "name", "email"],
        )),
        ..CapabilityV2::new("reserve_table", "Reserve a table", CapabilityEffect::NonIdempotent)
            .with_min_trust(TrustTier::Known)
    };
    PackBundle::new(
        "waitlist",
        version,
        PackPublisher::new(PUBLISHER).named("Tables"),
        "A waiting list, the menu and table reservations for restaurants",
    )
    .capability(join, HandlerConfig::Form)
    .capability(menu, HandlerConfig::Listing { content: ContentKind::Product })
    .capability(availability, HandlerConfig::Booking { operation: BookingOperation::Availability })
    .capability(reserve, HandlerConfig::Booking { operation: BookingOperation::Book })
    .playbook(PackPlaybook {
        id: "waitlist-capture".into(),
        job: "capture".into(),
        agents: vec![AgentRole::Concierge],
        text: "---\nname: waitlist-capture\ndescription: \"Offer the waiting list when the evening is full.\"\ntags: [website, \"job:capture\", \"agent:concierge\"]\n---\n\n# Waiting list\n\nWhen no table is open, offer `join_waitlist`; never promise a table.\n".into(),
    })
    .driver(PackDriver {
        id: "tables-pos".into(),
        manifest: "# The restaurant's point of sale.\nid = \"tables-pos\"\ncommand = \"mcp-tables-pos\"\npartitions = [\"back-office\"]\nsandbox = \"os\"\n\n[tools]\ntables_list = \"read\"\ntables_hold = \"schedule_with_others\"\n".into(),
    })
}

async fn sealed(bundle: PackBundle, key: &Ed25519DocumentSigner) -> Vec<u8> {
    bundle.seal(key).await.unwrap().to_json().unwrap()
}

/// Every vector file, by name, and the cases the conformance test runs over them.
async fn vectors() -> Vec<(String, Vec<u8>)> {
    let key = publisher_key();
    let mut files = Vec::new();
    let mut add = |name: &str, bytes: Vec<u8>| files.push((name.to_string(), bytes));

    add("trust.json", {
        let mut bytes = serde_json::to_vec_pretty(&trust()).unwrap();
        bytes.push(b'\n');
        bytes
    });
    add("waitlist-1.0.0.pack.json", sealed(waitlist("1.0.0"), &key).await);
    let mut next = waitlist("1.1.0");
    next.capabilities[0].description = "Join the waiting list for tonight or tomorrow".into();
    add("waitlist-1.1.0.pack.json", sealed(next, &key).await);
    let mut changed = waitlist("1.0.0");
    changed.description = "The same version with other content".into();
    add("republished.pack.json", sealed(changed, &key).await);

    add("unsigned.pack.json", waitlist("1.0.0").to_json().unwrap());
    let signed: Value = serde_json::from_slice(&sealed(waitlist("1.0.0"), &key).await).unwrap();
    let mut tampered = signed.clone();
    tampered["capabilities"][0]["minTrust"] = json!("public");
    tampered["capabilities"][3]["minTrust"] = json!("public");
    add("tampered-content.pack.json", pretty(&tampered));
    let mut redigested = tampered.clone();
    let mut content = redigested.clone();
    content.as_object_mut().unwrap().remove("digest");
    content.as_object_mut().unwrap().remove("proof");
    redigested["digest"] = json!(adk_awp::signing::jws::document_digest(&content).unwrap());
    add("tampered-digest.pack.json", pretty(&redigested));
    let thief = Ed25519DocumentSigner::from_seed(KID, [0x58; 32]).unwrap();
    add("wrong-key.pack.json", sealed(waitlist("1.0.0"), &thief).await);
    let stranger = Ed25519DocumentSigner::from_seed("stranger-2026", [0x53; 32]).unwrap();
    let mut theirs = waitlist("1.0.0");
    theirs.publisher = PackPublisher::new("stranger.example");
    add("unknown-publisher.pack.json", sealed(theirs, &stranger).await);

    let mut unsandboxed = waitlist("1.0.0");
    unsandboxed.drivers[0].manifest =
        unsandboxed.drivers[0].manifest.replace("sandbox = \"os\"", "sandbox = \"unsandboxed\"");
    add("unsandboxed-driver.pack.json", sealed(unsandboxed, &key).await);
    let mut undeclared = waitlist("1.0.0");
    undeclared.drivers[0].manifest.push_str("tables_refund = \"other\"\n");
    add("undeclared-effect.pack.json", sealed(undeclared, &key).await);
    let mut understated = waitlist("1.0.0");
    understated.capabilities[0].effect = CapabilityEffect::ReadOnly;
    add("understated-effect.pack.json", sealed(understated, &key).await);
    let mut builtin_capability = waitlist("1.0.0");
    builtin_capability.capabilities[3].name = "book".into();
    let handler = builtin_capability.handlers.remove("reserve_table").unwrap();
    builtin_capability.handlers.insert("book".into(), handler);
    add("takes-a-built-in-capability.pack.json", sealed(builtin_capability, &key).await);
    let mut builtin_name = waitlist("9.0.0");
    builtin_name.name = "booking".into();
    add("takes-a-built-in-name.pack.json", sealed(builtin_name, &key).await);
    let mut future = signed.clone();
    future["format"] = json!("adk-website.pack/2");
    add("future-format.pack.json", pretty(&future));
    let mut extra = signed;
    extra["install"] = json!("curl https://tables.example/setup.sh | sh");
    add("unknown-member.pack.json", pretty(&extra));

    let case = |name: &str, steps: &[(&str, &str)]| {
        json!({
            "name": name,
            "steps": steps.iter().map(|(file, expect)| json!({ "install": file, "expect": expect })).collect::<Vec<_>>(),
        })
    };
    let cases = json!({
        "trust": "trust.json",
        "outcomes": "new, upgrade and unchanged install; any other is the refusal's code",
        "cases": [
            case("a trusted pack installs once", &[("waitlist-1.0.0.pack.json", "new"), ("waitlist-1.0.0.pack.json", "unchanged")]),
            case("a newer version upgrades", &[("waitlist-1.0.0.pack.json", "new"), ("waitlist-1.1.0.pack.json", "upgrade")]),
            case("an older version is a downgrade", &[("waitlist-1.1.0.pack.json", "new"), ("waitlist-1.0.0.pack.json", "downgrade")]),
            case("a version never changes", &[("waitlist-1.0.0.pack.json", "new"), ("republished.pack.json", "republished")]),
            case("unsigned", &[("unsigned.pack.json", "unsigned")]),
            case("content changed after signing", &[("tampered-content.pack.json", "tampered")]),
            case("content and digest changed after signing", &[("tampered-digest.pack.json", "tampered")]),
            case("signed with another key under a trusted id", &[("wrong-key.pack.json", "tampered")]),
            case("a publisher the site does not trust", &[("unknown-publisher.pack.json", "unknown_publisher")]),
            case("a driver outside the sandbox", &[("unsandboxed-driver.pack.json", "invalid")]),
            case("a driver tool with no effect class", &[("undeclared-effect.pack.json", "invalid")]),
            case("a capability declared weaker than its handler", &[("understated-effect.pack.json", "invalid")]),
            case("a built-in pack's capability", &[("takes-a-built-in-capability.pack.json", "conflict")]),
            case("a built-in pack's name", &[("takes-a-built-in-name.pack.json", "conflict")]),
            case("a later format", &[("future-format.pack.json", "format")]),
            case("a member the format does not know", &[("unknown-member.pack.json", "malformed")]),
        ],
    });
    add("cases.json", pretty(&cases));
    files
}

fn pretty(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packs")
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[tokio::test]
async fn the_vectors_are_the_generated_ones() {
    let record = std::env::var("ADK_WEBSITE_PACKS_RECORD").is_ok_and(|v| v == "1");
    for (name, bytes) in vectors().await {
        let path = fixtures().join(&name);
        if record {
            std::fs::create_dir_all(fixtures()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            continue;
        }
        let shipped = String::from_utf8(fixture(&name)).unwrap().replace("\r\n", "\n");
        assert_eq!(
            shipped,
            String::from_utf8(bytes).unwrap(),
            "{name} is not the generated vector: run with ADK_WEBSITE_PACKS_RECORD=1"
        );
    }
}

/// The conformance cases over a fresh project each, from the fixture files alone.
#[test]
fn every_conformance_case_installs_or_refuses_as_expected() {
    let cases: Value = serde_json::from_slice(&fixture("cases.json")).unwrap();
    let trust = fixture(cases["trust"].as_str().unwrap());
    for case in cases["cases"].as_array().unwrap() {
        let project = tempfile::tempdir().unwrap();
        let packs = PackDir::new(project.path());
        std::fs::create_dir_all(packs.dir()).unwrap();
        std::fs::write(packs.trust_file(), &trust).unwrap();
        for step in case["steps"].as_array().unwrap() {
            let file = step["install"].as_str().unwrap();
            let outcome = match packs.install(&fixture(file)) {
                Ok(installed) => match installed.admission {
                    Admission::New => "new".to_string(),
                    Admission::Upgrade { .. } => "upgrade".to_string(),
                    Admission::Unchanged => "unchanged".to_string(),
                },
                Err(error) => error.code().to_string(),
            };
            assert_eq!(outcome, step["expect"].as_str().unwrap(), "{}: {file}", case["name"]);
        }
        // Whatever was refused left nothing behind, and what installed loads.
        let loaded = packs.load().unwrap();
        assert!(loaded.refused().is_empty(), "{}: {:?}", case["name"], loaded.refused());
    }
}

#[test]
fn an_installed_pack_is_loaded_upgraded_and_removed_with_its_driver_manifests() {
    let project = tempfile::tempdir().unwrap();
    let packs = PackDir::new(project.path());
    let refused = packs.install(&fixture("waitlist-1.0.0.pack.json")).unwrap_err();
    assert_eq!(refused.code(), "unknown_publisher", "nothing is trusted until the owner says so");

    let key = TrustedKey::new(PUBLISHER, KID, publisher_key().public_key());
    assert!(packs.trust_key(key.clone()).unwrap());
    assert!(!packs.trust_key(key).unwrap(), "trusted once");
    let builtin = adk_website::packs::builtin::builtin_trust().keys.remove(0);
    assert_eq!(packs.trust_key(builtin).unwrap_err().code(), "invalid", "the test key is public");

    let installed = packs.install(&fixture("waitlist-1.0.0.pack.json")).unwrap();
    assert_eq!(installed.admission, Admission::New);
    let manifest = packs.drivers_dir("waitlist").join("tables-pos.toml");
    assert_eq!(installed.drivers, std::slice::from_ref(&manifest));
    let mounted = adk_kernel::drivers::DriverManifest::from_toml(
        &std::fs::read_to_string(&manifest).unwrap(),
    )
    .unwrap();
    assert_eq!(mounted.id, "tables-pos");
    assert_eq!(
        std::fs::read(packs.bundle_file("waitlist")).unwrap(),
        fixture("waitlist-1.0.0.pack.json"),
        "the bundle is kept byte for byte as signed"
    );

    let set = packs.load().unwrap();
    let pack = set.get("waitlist").unwrap();
    assert_eq!(pack.origin, PackOrigin::Installed { file: packs.bundle_file("waitlist") });
    assert_eq!(set.packs().len(), 7);
    let served: Vec<String> = set
        .served(&adk_website::kinds::template(SiteKind::Business))
        .into_iter()
        .map(|(declaration, _)| declaration.name)
        .collect();
    for name in
        ["submit_inquiry", "join_waitlist", "list_menu", "table_availability", "reserve_table"]
    {
        assert!(served.contains(&name.to_string()), "{name}: {served:?}");
    }

    let upgraded = packs.install(&fixture("waitlist-1.1.0.pack.json")).unwrap();
    assert_eq!(upgraded.admission, Admission::Upgrade { from: "1.0.0".into() });
    assert_eq!(packs.load().unwrap().get("waitlist").unwrap().bundle().version, "1.1.0");

    assert_eq!(packs.remove("booking").unwrap_err().code(), "built_in");
    assert_eq!(packs.remove("nothing").unwrap_err().code(), "not_installed");
    packs.remove("waitlist").unwrap();
    assert!(!manifest.exists() && !packs.bundle_file("waitlist").exists());
    assert!(packs.load().unwrap().get("waitlist").is_none());
}

#[test]
fn a_bundle_changed_after_installation_or_a_publisher_no_longer_trusted_is_not_loaded() {
    let project = tempfile::tempdir().unwrap();
    let packs = PackDir::new(project.path());
    packs.trust_key(TrustedKey::new(PUBLISHER, KID, publisher_key().public_key())).unwrap();
    packs.install(&fixture("waitlist-1.0.0.pack.json")).unwrap();

    let tampered = fixture("tampered-content.pack.json");
    std::fs::write(packs.bundle_file("waitlist"), &tampered).unwrap();
    let set = packs.load().unwrap();
    assert!(set.get("waitlist").is_none());
    assert_eq!(set.refused()[0].error.code(), "tampered");

    std::fs::write(packs.bundle_file("waitlist"), fixture("waitlist-1.0.0.pack.json")).unwrap();
    std::fs::write(packs.trust_file(), "{\"keys\": []}\n").unwrap();
    let set = packs.load().unwrap();
    assert!(set.get("waitlist").is_none());
    assert_eq!(set.refused()[0].error.code(), "unknown_publisher");

    // A file named for another pack is not loaded either.
    packs.trust_key(TrustedKey::new(PUBLISHER, KID, publisher_key().public_key())).unwrap();
    std::fs::rename(packs.bundle_file("waitlist"), packs.bundle_file("menu")).unwrap();
    assert_eq!(packs.load().unwrap().refused()[0].error.code(), "invalid");
}

fn loaded(set: PackSet, file: &str) -> PackSet {
    let pack = PackBundle::verify(&fixture(file), &trust()).unwrap();
    let origin = PackOrigin::Installed { file: PathBuf::from(file) };
    set.with(LoadedPack { pack, origin }).unwrap().0
}

#[tokio::test]
async fn the_packs_playbooks_join_their_agents_workspaces_and_leave_with_the_pack() {
    let root = tempfile::tempdir().unwrap();
    let with = loaded(PackSet::builtin().unwrap(), "waitlist-1.0.0.pack.json");
    let written = with.install_playbooks(root.path()).await.unwrap();
    let path = root.path().join("concierge/.skills/packs/waitlist/waitlist-capture.md");
    assert_eq!(written, std::slice::from_ref(&path));
    let skill =
        adk_skill::parse_skill_markdown(&path, &std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(skill.name, "waitlist-capture");

    PackSet::builtin().unwrap().install_playbooks(root.path()).await.unwrap();
    assert!(!path.exists(), "a removed pack's playbook goes with it");
}

#[tokio::test]
async fn the_edge_serves_a_packs_capabilities_with_the_handlers_it_configures() {
    let site = Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let tonight = chrono::Utc::now() + chrono::Duration::hours(6);
    let slot = Slot { id: "table-4".into(), starts_at: tonight, minutes: 90, price: None };
    let shop = Shop {
        calendar: Some(Arc::new(InMemoryCalendar::new([slot]))),
        packs: Some(loaded(PackSet::builtin().unwrap(), "waitlist-1.0.0.pack.json")),
        ..Shop::default()
    };
    let edge = edge_shop(&site, "juniper.example", shop).await;
    let names = |manifest: &Value| -> Vec<String> {
        manifest["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect()
    };
    let (status, _, manifest) = edge.send(edge.get("/awp/v2/manifest")).await;
    assert_eq!(status, StatusCode::OK);
    let public = names(&manifest);
    for name in ["submit_inquiry", "join_waitlist", "list_menu", "table_availability"] {
        assert!(public.contains(&name.to_string()), "{name}: {public:?}");
    }
    assert!(!public.contains(&"reserve_table".to_string()), "reservations are for known agents");
    let (_, _, known) = edge.send(edge.signed(edge.get("/awp/v2/manifest"))).await;
    assert!(names(&known).contains(&"reserve_table".to_string()));

    // The bookings handler lists the calendar's slots for the pack's own name.
    let (status, _, open) =
        edge.send(edge.call("table_availability", "a-1", None, &json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{open}");
    assert_eq!(open["slots"][0]["id"], "table-4");

    // The forms handler, under the pack's own consent scope (A-I8).
    let join = json!({ "email": "otieno@mail.example", "party": 4 });
    let (status, _, refused) = edge.send(edge.call("join_waitlist", "w-1", None, &join)).await;
    assert_eq!((status.as_u16(), &refused["error"]["code"]), (451, &json!("consent_required")));
    let receipt = edge.consent("waitlist").await;
    let (status, _, joined) =
        edge.send(edge.call("join_waitlist", "w-1", Some(&receipt), &join)).await;
    assert_eq!(status, StatusCode::OK, "{joined}");
    let reference = joined["reference"].as_str().unwrap().to_string();
    let lead = edge.stores.leads.get(&LeadRef(reference)).await.unwrap().unwrap();
    assert_eq!(lead.answers["capability"].as_str(), "join_waitlist");
    assert_eq!(lead.answers["party"].as_str(), "4");

    // A reservation takes the slot once.
    let contact = edge.consent("contact").await;
    let reserve = json!({ "slot": "table-4", "name": "Otieno", "email": "otieno@mail.example" });
    let (status, _, booked) =
        edge.send(edge.signed(edge.call("reserve_table", "r-1", Some(&contact), &reserve))).await;
    assert_eq!(status, StatusCode::OK, "{booked}");
    assert_eq!(booked["booked"], true);
    let (_, _, after) = edge.send(edge.call("table_availability", "a-2", None, &json!({}))).await;
    assert_eq!(after["slots"], json!([]));
}

async fn http(addr: SocketAddr, path: &str) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text.split(' ').nth(1).and_then(|code| code.parse().ok()).unwrap_or(0);
    let body = text.split_once("\r\n\r\n").map(|(_, body)| body.to_string()).unwrap_or_default();
    (status, body)
}

#[tokio::test]
async fn a_served_site_loads_its_installed_packs_and_not_a_tampered_one() {
    let dir = tempfile::tempdir().unwrap();
    let options = ScaffoldOptions {
        name: "Juniper Bakery".into(),
        domain: "juniper.example".into(),
        kind: SiteKind::Business,
        languages: vec!["en".parse().unwrap()],
        theme: ThemeId::Paper,
        currency: "KES".into(),
    };
    scaffold(dir.path(), &options).await.unwrap();
    let packs = PackDir::new(dir.path());
    packs.trust_key(TrustedKey::new(PUBLISHER, KID, publisher_key().public_key())).unwrap();
    packs.install(&fixture("waitlist-1.0.0.pack.json")).unwrap();
    // A second pack's file, changed after it was installed.
    let mut menu: Value = serde_json::from_slice(&fixture("tampered-content.pack.json")).unwrap();
    menu["name"] = json!("menu");
    std::fs::write(packs.bundle_file("menu"), pretty(&menu)).unwrap();

    let mut serve_options = ServeOptions::new(dir.path(), common::OWNER);
    serve_options.edge = "127.0.0.1:0".parse().unwrap();
    serve_options.kernel = "127.0.0.1:0".parse().unwrap();
    serve_options.site.idle = Some(Duration::from_millis(200));
    let served = serve(serve_options).await.unwrap();
    let site = &served.sites[0];
    assert!(site.packs.get("waitlist").is_some());
    assert!(site.packs.get("menu").is_none());
    assert_eq!(site.packs.refused()[0].error.code(), "tampered");

    let (status, body) = http(served.edge_addr, "/awp/v2/manifest").await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"join_waitlist\"") && body.contains("\"list_menu\""), "{body}");
    served.shutdown().await.unwrap();
}
