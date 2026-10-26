//! S18 — publishing at speed (spec 3.5 §5 S18, §7.8.2, §7.8.3, A-I23, A-I24, A-I25).
//!
//! A bakery's owner, on the WhatsApp number paired to them, adds a product with three photos
//! for launch at 09:00; two price changes follow, of 5% and 15%; an email from an unpaired
//! account asks to change a price. A newsroom's reporter files a breaking story that an
//! editor and a publisher approve on their phones, then corrects it; the newsroom publishes to
//! `DirectoryHost` and to the reference CDN host (an object store behind a pointer).
//!
//! Pass condition: the product is live at 09:00 in one signed release covering its page,
//! category pages, sitemap, product feed and machine view, uploading only changed files; the
//! 5% change goes live and the 15% change is held; the story is live under 60 s after the
//! publisher's approval on `DirectoryHost` and the reference CDN host; the correction note is
//! visible and both releases are in the log; the unpaired email produced only a signal
//! (A-I24, A-I25).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::principal::SubjectRef;
use adk_core::{Clock, ManualClock};
use adk_harness::HarnessMode;
use adk_harness::website::{AuthoringRoute, AuthoringRouter};
use adk_kernel::channels::{
    ChannelBinding, ChannelBindingStore, ChannelInbound, InMemoryChannelBindingStore,
};
use adk_kernel::charter::{RoleApproval, SignalFilter, SignalStore};
use adk_model::ScriptedResponse;
use adk_session::CreateRequest;
use adk_site::batch::Update;
use adk_site::content::ContentStore;
use adk_site::host::SiteHost;
use adk_site::live::InMemoryLiveStore;
use adk_site::pipeline::{ApprovalDecision, Change, EntryPoint, PipelineOutcome};
use adk_site::release::manifest_path;
use adk_site::updates::UpdateEngine;
use adk_website::kinds::CharterInputs;
use adk_website::team::AgentRole;
use anyhow::{Result, anyhow};
use awp_types::{
    ContentId, ContentItem, ContentKind, ContentState, Markdown, ReleaseManifest, Role, RoleHolder,
    SiteKind,
};
use base64::Engine as _;
use chrono::{TimeZone, Utc};
use serde_json::json;

use crate::checks::Checks;
use crate::fixture::*;
use crate::live::Mode;

const SHOP: &str = "juniper.example";
const NEWS: &str = "nakuru-today.example";
const OWNER_PHONE: &str = "+254700000001";
const PRODUCT: &str = "products/seeded-rye/en";

/// A small PNG photo, `n` its colour.
fn photo(n: u8) -> String {
    let mut image = image::RgbImage::new(4, 3);
    for pixel in image.pixels_mut() {
        *pixel = image::Rgb([n.wrapping_mul(60), 120, 200]);
    }
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .expect("a PNG encodes");
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn story(body: &str) -> ContentItem {
    ContentItem {
        id: ContentId("articles/flood/en".into()),
        kind: ContentKind::Article,
        slug: "flood".into(),
        language: "en".parse().expect("a language"),
        fields: [
            ("title".to_string(), json!("Flooding closes the Nakuru road")),
            ("summary".to_string(), json!("Breaking.")),
            ("byline".to_string(), json!("Reporter")),
            ("goal".to_string(), json!("readers")),
        ]
        .into_iter()
        .collect(),
        volatile: Vec::new(),
        body: Some(Markdown(body.into())),
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Draft,
    }
}

/// Runs the newsroom half on `site`: the story through its chain, then the correction.
/// Returns the seconds from the publisher's approval to live, and whether the correction note
/// is served and both releases verify.
async fn newsroom(site: &Site) -> Result<(f64, bool, u64)> {
    let pipeline = site.runtime.pipeline();
    let reporter = "reporter";
    let draft = pipeline
        .draft(Change {
            items: vec![story("The Nakuru–Nairobi road is closed at Gilgil after overnight rain.")],
            media: Vec::new(),
            entry: EntryPoint::PairedDevice { device: "reporter-phone".into() },
            proposer: reporter.into(),
            job: "newsroom".into(),
            action_class: "publish".into(),
        })
        .await?;
    let preview = pipeline.preview(&draft).await?;
    let ticket = preview.staged.digest.clone();
    let approvals = site.runtime.approvals();
    approvals
        .approve(&ticket, RoleApproval::new(Role::Editor, SubjectRef::human("editor"), Utc::now()))
        .await;
    let waiting =
        matches!(pipeline.approve(&draft, &preview).await?, ApprovalDecision::Pending { .. });
    approvals
        .approve(
            &ticket,
            RoleApproval::new(Role::Publisher, SubjectRef::human("publisher"), Utc::now()),
        )
        .await;
    let approved = Instant::now();
    let ApprovalDecision::Approved { decision, .. } = pipeline.approve(&draft, &preview).await?
    else {
        return Err(anyhow!("the editor and the publisher satisfy the chain"));
    };
    let outcome = pipeline.complete(&draft, decision).await?;
    let host = site.runtime.host().clone();
    let live = host.fetch("/articles/flood/").await?.is_some()
        && matches!(outcome, PipelineOutcome::Published(_));
    let seconds = approved.elapsed().as_secs_f64();
    anyhow::ensure!(waiting && live, "the story waited for the publisher, then went live");
    let mut corrected = pipeline
        .correct(
            &ContentId("articles/flood/en".into()),
            |item| {
                item.body = Some(Markdown("The road is closed at Gilgil and at Naivasha.".into()))
            },
            "The first version named only Gilgil.",
            reporter,
            EntryPoint::PairedDevice { device: "reporter-phone".into() },
        )
        .await?;
    // A correction goes through the same chain.
    if let PipelineOutcome::Pending { draft, preview, .. } = &corrected {
        let ticket = preview.staged.digest.clone();
        approvals
            .approve(
                &ticket,
                RoleApproval::new(Role::Editor, SubjectRef::human("editor"), Utc::now()),
            )
            .await;
        approvals
            .approve(
                &ticket,
                RoleApproval::new(Role::Publisher, SubjectRef::human("publisher"), Utc::now()),
            )
            .await;
        if let ApprovalDecision::Approved { decision, .. } =
            pipeline.approve(draft, preview).await?
        {
            corrected = pipeline.complete(draft, decision).await?;
        }
    }
    let page = host
        .fetch("/articles/flood/")
        .await?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let seq = match corrected {
        PipelineOutcome::Published(p) => p.staged.manifest.seq,
        _ => 0,
    };
    let note = page.contains("The first version named only Gilgil.") && page.contains("Naivasha");
    Ok((seconds, note, seq))
}

/// The reference CDN host: an object store behind a pointer the CDN follows.
#[cfg(feature = "cdn")]
fn cdn_host() -> Arc<dyn SiteHost> {
    use adk_site::host::object::{InMemoryObjectStore, ObjectStoreHost};
    Arc::new(ObjectStoreHost::prefix_pointer(Arc::new(InMemoryObjectStore::new())))
}

#[cfg(not(feature = "cdn"))]
fn cdn_host() -> Arc<dyn SiteHost> {
    unreachable!("the CDN host runs only with the `cdn` feature")
}

/// Runs S18.
#[allow(clippy::too_many_lines)]
pub async fn run(mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();

    // The bakery: launched yesterday, with its clock at 07:30 on launch day.
    let clock = ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 27, 7, 30, 0).single().ok_or_else(|| anyhow!("a date"))?,
    );
    let shared: Arc<dyn Clock> = Arc::new(clock.clone());
    let mut shop = Site::with(
        SiteKind::Ecommerce,
        "Juniper Bakery",
        SHOP,
        &["en"],
        Some(shared.clone()),
        |_| {},
    )
    .await?;
    let rye_loaf = ContentItem {
        id: ContentId("products/rye-loaf/en".into()),
        kind: ContentKind::Product,
        slug: "rye-loaf".into(),
        language: "en".parse()?,
        fields: [
            ("title".to_string(), json!("Rye loaf")),
            ("summary".to_string(), json!("Dark rye, baked every morning.")),
            ("sku".to_string(), json!("rye-loaf")),
            ("price".to_string(), json!(450)),
            ("currency".to_string(), json!("KES")),
            ("goal".to_string(), json!("sales")),
        ]
        .into_iter()
        .collect(),
        volatile: Vec::new(),
        body: Some(Markdown("Dark rye, baked every morning.\n".into())),
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Draft,
    };
    let charter = shop.launch("Juniper Bakery", "KES", vec![rye_loaf]).await?;

    // The owner's WhatsApp number is paired: their message reaches the Steward, entering
    // through the paired device.
    let bindings = Arc::new(InMemoryChannelBindingStore::new());
    bindings
        .put_binding(ChannelBinding::new("whatsapp", OWNER_PHONE, "wa-1", OWNER, Utc::now()))
        .await?;
    let router = AuthoringRouter::new(bindings.clone());
    let message = "New loaf for launch at 9 today: Seeded rye, 520 KES. Three photos attached.";
    let route = router
        .route(&ChannelInbound::text("whatsapp", OWNER_PHONE, "wa-1", "m-1", message))
        .await?;
    let entry_state = route.session_state();
    checks.check(
        "the paired owner's message reaches the Steward through the paired device (A-I24)",
        matches!(&route, AuthoringRoute::Owner { user_id, .. } if user_id == OWNER)
            && entry_state.is_some(),
        format!("{route:?}"),
    );
    let prompter = ScriptedPrompter::new();
    prompter.approve_once();
    let launch_at = "2026-10-27T09:00:00Z";
    let (steward, model) = shop.back_office(
        &mode,
        "steward",
        &charter,
        vec![
            call("content_draft", json!({
                "kind": "product", "slug": "seeded-rye", "language": "en",
                "fields": { "title": "Seeded rye", "summary": "Rye with sunflower and pumpkin seeds.", "sku": "seeded-rye", "price": 520, "currency": "KES", "goal": "sales" },
                "body": "Rye with sunflower and pumpkin seeds, baked this morning.",
                "schedule": { "publishAt": launch_at },
                "job": "onboarding"
            })),
            ScriptedResponse::text("Drafted; adding the photos."),
        ],
        |b| {
            b.instruction(format!(
                "{}\n\nThe owner wrote on WhatsApp: \"{message}\". Draft the product with \
                 content_draft (kind product, slug seeded-rye, language en, fields title, summary, \
                 sku seeded-rye, price 520, currency KES, goal sales, a one-line body, schedule \
                 publishAt {launch_at}, job onboarding). The message's three photos reach the \
                 draft through the channel; when the owner says they are attached, publish the \
                 draft with site_publish.",
                AgentRole::Steward.prompt()
            ))
            .mode(HarnessMode::Ask)
            .prompter(prompter.clone())
        },
    )?;
    let owner = user(OWNER);
    let session_id = format!("wa-{}", uuid::Uuid::new_v4());
    let mut state = HashMap::new();
    if let Some((key, value)) = entry_state {
        state.insert(key, value);
    }
    shop.sessions
        .create(CreateRequest {
            app_name: shop.site.name.clone(),
            user_id: OWNER.into(),
            session_id: Some(session_id.clone()),
            state,
        })
        .await?;
    let session =
        steward.open_session(&owner, Some(&adk_core::SessionId::new(&session_id)?)).await?;
    let first = collect(steward.send(&owner, &session, message).await?).await?;
    let draft = response(&first, "content_draft", 0)
        .and_then(|r| r["draft"].as_str().map(str::to_string))
        .unwrap_or_default();
    if draft.is_empty() {
        println!("      the draft did not come back: {:?}", responses(&first));
    }
    // The WhatsApp adapter hands the message's photos to the product's owner channel, which
    // runs them through the back office's media intake into the Steward's draft (A-I24), and
    // asks the owner on the thread to confirm their descriptions.
    let channel = adk_website::channel::OwnerChannel::new(
        shop.runtime.clone(),
        AuthoringRouter::new(bindings.clone()),
    )?;
    let inbound = ChannelInbound::text("whatsapp", OWNER_PHONE, "wa-1", "m-1", message);
    let photos: Vec<adk_website::channel::ChannelPhoto> = (0..3u8)
        .map(|n| adk_website::channel::ChannelPhoto {
            name: format!("seeded-rye-{n}.png"),
            bytes: base64::engine::general_purpose::STANDARD
                .decode(photo(n))
                .unwrap_or_default()
                .into(),
            caption: (n == 0).then(|| "Seeded rye loaf on the counter".to_string()),
        })
        .collect();
    let attached =
        channel.attach(&inbound, &draft, &ContentId(PRODUCT.into()), "Seeded rye", photos).await;
    let attached = match attached {
        Ok(attached) => attached,
        Err(error) => {
            println!("      the photos were not attached: {error}");
            adk_website::channel::Attached { photos: Vec::new(), reply: String::new() }
        }
    };
    if mode.is_live() {
        println!("      the channel replies: {}", attached.reply);
    }
    model.push(call("site_publish", json!({ "draft": draft })));
    model.push(ScriptedResponse::text("Scheduled for 09:00."));
    let second =
        collect(steward.send(&owner, &session, "Photos are attached; schedule it.").await?).await?;
    if mode.is_live() {
        trace(&first);
        trace(&second);
    }
    let before = shop.host.fetch("/products/seeded-rye/").await?.is_none();
    checks.check(
        "the product with three photos waits for 09:00",
        attached.photos.len() == 3 && before,
        format!(
            "{} photos attached from the WhatsApp message; not served at 07:30; the publish {:?}",
            attached.photos.len(),
            response(&second, "site_publish", 0)
        ),
    );

    // The owner answers on WhatsApp: the second photo's description changes, the rest are
    // confirmed; the confirmations reach the scheduled item in the content store.
    let change = ChannelInbound::text(
        "whatsapp",
        OWNER_PHONE,
        "wa-1",
        "m-2",
        "2: Seeded rye, sliced on a board",
    );
    let changed = channel.reply(&change).await?;
    let yes = ChannelInbound::text("whatsapp", OWNER_PHONE, "wa-1", "m-3", "Yes");
    let confirmed = channel.reply(&yes).await?;
    channel.settle().await?;
    let stored = shop.store.get(&ContentId(PRODUCT.into())).await?;
    let alt_ok = stored.as_ref().is_some_and(|item| {
        item.media.len() == 3
            && item.media.iter().all(|m| m.alt_confirmed)
            && item.media.iter().any(|m| m.alt.as_deref() == Some("Seeded rye, sliced on a board"))
    });
    checks.check(
        "the owner confirms the photos' alt text from WhatsApp",
        changed.is_some() && confirmed.is_some() && alt_ok,
        format!(
            "{} confirmed on the thread; {} still waiting",
            stored.as_ref().map_or(0, |item| item.media.iter().filter(|m| m.alt_confirmed).count()),
            channel.waiting("wa-1").await.len()
        ),
    );

    // 09:00: one signed release with the product's page, the category pages, the sitemap, the
    // product feed and the machine view, uploading only what changed.
    clock.set(
        Utc.with_ymd_and_hms(2026, 10, 27, 9, 0, 0).single().ok_or_else(|| anyhow!("a date"))?,
    );
    let due = shop.runtime.pipeline().run_due(None).await?;
    let published = due.iter().find_map(|o| match o {
        PipelineOutcome::Published(p) => Some(p),
        _ => None,
    });
    let (seq, uploaded) = published
        .map(|p| {
            let uploaded =
                p.report.uploaded.iter().filter(|f| !f.starts_with("/.well-known/awp/")).cloned();
            (p.staged.manifest.seq, uploaded.collect::<BTreeSet<String>>())
        })
        .unwrap_or_default();
    let manifest: Option<ReleaseManifest> = match shop.host.fetch(&manifest_path(seq)).await {
        Ok(Some(bytes)) => serde_json::from_slice(&bytes).ok(),
        _ => None,
    };
    let files: BTreeSet<String> =
        manifest.map(|m| m.files.keys().cloned().collect()).unwrap_or_default();
    let covers = |p: &str| uploaded.contains(p);
    let feed = uploaded.iter().find(|p| p.ends_with(".xml") && p != &"/sitemap.xml").cloned();
    checks.check(
        "the product is live at 09:00 in one signed release with its page, category page, sitemap and machine view",
        due.len() == 1
            && covers("/products/seeded-rye/index.html")
            && covers("/products/index.html")
            && covers("/sitemap.xml")
            && covers("/products/seeded-rye/index.awp.json")
            && verify_served(&shop.host, SHOP, &shop.release_key).await.ok() == Some(seq),
        format!("release {seq}: {uploaded:?}"),
    );
    let listed =
        match &feed {
            Some(path) => shop.host.fetch(path).await?.is_some_and(|bytes| {
                String::from_utf8_lossy(&bytes).contains("/products/seeded-rye/")
            }),
            None => false,
        };
    checks.check(
        "the release carries the product feed",
        feed.as_deref() == Some("/products/feed.xml") && listed,
        format!("{feed:?}, listing the new product: {listed}"),
    );
    let untouched = [
        "/about/index.html",
        "/index.html",
        "/products/rye-loaf/index.html",
        "/privacy/index.html",
    ];
    checks.check(
        "the release uploads only changed files",
        !uploaded.is_empty() && untouched.iter().all(|p| !uploaded.contains(*p)),
        format!("{} changed files of {} in its manifest; the home, about, privacy and rye-loaf pages were not uploaded", uploaded.len(), files.len()),
    );

    // Two price changes through the update engine: 5% acts, 15% is held (A-I25).
    let engine = UpdateEngine::builder(
        shop.runtime.shared_pipeline(),
        Arc::new(InMemoryLiveStore::default()),
    )
    .build()?;
    let rye =
        shop.store.get(&ContentId(PRODUCT.into())).await?.ok_or_else(|| anyhow!("the product"))?;
    let mut five = rye.clone();
    five.fields.insert("price".into(), json!(546));
    engine
        .submit(Update {
            item: five,
            at: clock.now(),
            entry: EntryPoint::PairedDevice { device: "wa-1".into() },
            proposer: OWNER.into(),
            trigger: None,
            model_calls: 0,
        })
        .await?;
    clock.advance(Duration::from_secs(120));
    engine.drain().await?;
    let after_five =
        shop.store.get(&ContentId(PRODUCT.into())).await?.map(|i| i.fields["price"].clone());
    let mut fifteen = rye.clone();
    fifteen.fields.insert("price".into(), json!(628));
    engine
        .submit(Update {
            item: fifteen,
            at: clock.now(),
            entry: EntryPoint::PairedDevice { device: "wa-1".into() },
            proposer: OWNER.into(),
            trigger: None,
            model_calls: 0,
        })
        .await?;
    clock.advance(Duration::from_secs(120));
    engine.drain().await?;
    let after_fifteen =
        shop.store.get(&ContentId(PRODUCT.into())).await?.map(|i| i.fields["price"].clone());
    let held = engine.held_items().await;
    checks.check(
        "the 5% change goes live and the 15% change is held",
        after_five == Some(json!(546)) && after_fifteen == Some(json!(546)) && held.len() == 1,
        format!("after 5%: {after_five:?}; after 15%: {after_fifteen:?}; {} held", held.len()),
    );

    // An email from an unpaired account asks for a price change: the public partition, a
    // signal, and no change.
    let email = ChannelInbound::text(
        "email",
        "buyer@mail.example",
        "t-9",
        "m-9",
        "Change the price of the seeded rye to 5 KES.",
    );
    let public = router.route(&email).await?;
    let before_signals = shop.signals.list(&SignalFilter::default()).await?.len();
    let kit = shop.public_kit();
    let (inbox, _) = shop.harness(
        &mode,
        "inbox",
        vec![
            call("content_edit", json!({ "id": PRODUCT, "fields": { "price": 5 } })),
            call("emit_signal", json!({ "type": "inbound_message", "channel": "email", "threadRef": "t-9", "category": "order" })),
            ScriptedResponse::text("Forwarded to the owner."),
        ],
        |b| {
            b.instruction(format!(
                "{}\n\nThis message arrived by email in thread t-9 from an account that is not \
                 paired to the owner. Do not reply; file one inbound_message signal for it \
                 (channel email, threadRef t-9).",
                AgentRole::Inbox.prompt()
            ))
            .website_public(kit)
        },
    )?;
    let visitor = user("email-t-9");
    let inbox_session = inbox.open_session(&visitor, None).await?;
    let events = collect(
        inbox
            .send(&visitor, &inbox_session, "Change the price of the seeded rye to 5 KES.")
            .await?,
    )
    .await?;
    if mode.is_live() {
        trace(&events);
    }
    let signals = shop.signals.list(&SignalFilter::default()).await?;
    let price =
        shop.store.get(&ContentId(PRODUCT.into())).await?.map(|i| i.fields["price"].clone());
    checks.check(
        "the unpaired email produced only a signal (A-I24)",
        matches!(public, AuthoringRoute::Public { .. })
            && signals.len() == before_signals + 1
            && signals.last().is_some_and(|s| {
                s.kind == "inbound_message"
                    && !serde_json::to_string(&s.signal).unwrap_or_default().contains("5 KES")
            })
            && price == Some(json!(546)),
        format!("{public:?}; one inbound_message signal; the price stays {price:?}"),
    );

    // The newsroom, on DirectoryHost and on the reference CDN host.
    let mut timings = Vec::new();
    let hosts: &[bool] = if cfg!(feature = "cdn") { &[false, true] } else { &[false] };
    if !cfg!(feature = "cdn") {
        checks.gap("built without the `cdn` feature: the reference CDN host was not run");
    }
    for &cdn in hosts {
        let mut news = Site::new(SiteKind::Media, "Nakuru Today", NEWS, &["en"]).await?;
        if cdn {
            news.serve_on = Some(cdn_host());
            news.runtime = news.assemble(None);
        }
        let mut inputs = CharterInputs::new(
            news.site.id,
            "Nakuru Today",
            OWNER,
            "News for Nakuru.",
            vec!["en".parse()?],
            "KES",
        );
        inputs.people.push(RoleHolder { principal: "editor".into(), roles: vec![Role::Editor] });
        inputs
            .people
            .push(RoleHolder { principal: "publisher".into(), roles: vec![Role::Publisher] });
        let charter = news.template.draft_charter(&news.team, &inputs);
        news.commit(&charter, &[(Role::Owner, OWNER)]).await?;
        let items = adk_website::onboarding::build::SitePlan::new(&charter, "Nakuru Today").items(
            &charter,
            &[],
            &["KE".to_string()],
        );
        adk_website::onboarding::build::stage_release_one(
            &news.runtime,
            items,
            &AgentRole::Steward.principal(&news.site),
        )
        .await?;
        let (seconds, note, corrected) = newsroom(&news).await?;
        let log = news.log.size().await;
        timings.push((
            if cdn { "reference CDN host" } else { "DirectoryHost" },
            seconds,
            note,
            corrected,
            log,
        ));
    }
    checks.check(
        "the story is live under 60 s after the publisher's approval on DirectoryHost and the reference CDN host",
        timings.iter().all(|(_, s, ..)| *s < 60.0),
        timings.iter().map(|(h, s, ..)| format!("{h}: {s:.2} s")).collect::<Vec<_>>().join("; "),
    );
    checks.check(
        "the correction note is visible and both releases are in the log",
        timings
            .iter()
            .all(|(_, _, note, corrected, log)| *note && *corrected >= 2 && *log >= *corrected),
        timings
            .iter()
            .map(|(h, _, _, c, l)| format!("{h}: correction is release {c}, the log holds {l}"))
            .collect::<Vec<_>>()
            .join("; "),
    );
    Ok(checks)
}
