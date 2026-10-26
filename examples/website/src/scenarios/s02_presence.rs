//! S2 — from an online presence (spec 3.5 §5 S2, §7.18 stages 2 and 4, A-I18, A-I19).
//!
//! No site, but a listing, a review page and a social profile exist. Discovery reads them,
//! records every fact it finds as `Assumed` with its source, and proposes the kind; the owner
//! confirms some facts in the interview and approves one review for quoting; release 1 is
//! built and published.
//!
//! Pass condition: the snapshot lists each found fact with its source as unconfirmed; only
//! confirmed facts appear on pages; review quotes appear only with the reviewer's public
//! attribution and the owner's approval.

use std::collections::BTreeSet;
use std::sync::Arc;

use adk_harness::{FactFilter, FactStatus, FactStore, HarnessMode};
use adk_model::ScriptedResponse;
use adk_site::host::SiteHost;
use adk_site::migrate::ReviewQuote;
use adk_site::migrate::http::{EgressScope, HttpClient};
use adk_website::onboarding::StartingPoint;
use adk_website::onboarding::build::{SitePlan, known_facts};
use adk_website::onboarding::discovery::{Discovery, OwnerCard, record_facts};
use anyhow::{Result, anyhow};
use awp_types::{ContentItem, FactRef, Markdown, Role, SiteKind};
use serde_json::json;

use crate::checks::Checks;
use crate::fixture::legacy::{LegacyServer, acting};
use crate::fixture::*;
use crate::live::Mode;

/// What the owner confirms in the interview.
const OWNER_SAYS: &str = "Yes, that's us. Our phone number is +254 700 000 001 and we open at \
                          seven on weekdays.";

/// Runs S2.
pub async fn run(mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let mut site =
        Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await?;
    let presence = LegacyServer::presence().await?;
    let profiles: Vec<String> = ["listing", "reviews", "social"]
        .iter()
        .map(|page| format!("{}/presence/{page}.html", presence.origin))
        .collect();

    // 2 Discover: read the presence politely; every fact is Assumed and cites its page.
    let http = HttpClient::builder()
        .egress(EgressScope::Only(BTreeSet::from([presence.origin.clone()])))
        .min_interval(std::time::Duration::ZERO)
        .build()?;
    let ctx = adk_site::migrate::SourceContext::new(acting(), Arc::new(http));
    let start = StartingPoint::Presence { profiles: profiles.clone() };
    let discovered = Discovery::new(start, "en".parse()?)?.run(&ctx).await?;
    let found = discovered.facts();
    let fact = |key: &str| found.iter().find(|f| f.key == key);
    let cited = found.iter().all(|f| {
        f.status == adk_site::gate::FactStatus::Assumed
            && profiles.contains(&f.source.url)
            && !f.source.locator.is_empty()
    });
    checks.check(
        "the snapshot lists each found fact with its source as unconfirmed",
        found.len() >= 5 && cited,
        found
            .iter()
            .map(|f| format!("{}={} ({})", f.key, f.value, f.source.locator))
            .collect::<Vec<_>>()
            .join("; "),
    );
    checks.check(
        "facts come from the listing, the review page and the social profile",
        fact("telephone").is_some_and(|f| f.source.url == profiles[0])
            && fact("aggregateRating").is_some_and(|f| f.source.url == profiles[1])
            && found.iter().any(|f| f.source.url == profiles[2]),
        "telephone from the listing, rating from the review page, description from the profile",
    );
    let kind = discovered.cards().into_iter().find_map(|card| match card {
        OwnerCard::ConfirmKind { proposal } => Some(proposal),
        _ => None,
    });
    if kind.is_none() {
        checks.gap(
            "kind detection proposes nothing from a presence: `Presence` records no `@type` \
             fact (the type is only in each fact's locator), so `KindEvidence::from_inventory` \
             finds no type and the owner is asked",
        );
    }
    let scope = site.partitions.fact_scope(OWNER);
    let recorded = record_facts(found, site.facts.as_ref(), &scope, "discovery").await?;
    let stored = site.facts.list(FactFilter::recall(&scope)).await?;
    checks.check(
        "every found fact is stored Assumed, citing its page",
        recorded.len() == found.len()
            && stored.iter().all(|f| {
                f.status == FactStatus::Assumed
                    && f.provenance.source_ids.iter().any(|s| profiles.contains(s))
            }),
        format!("{} facts stored", stored.len()),
    );

    // 4 Interview: the owner confirms the phone and the hours in their own words; the address
    // is never confirmed.
    let charter = site.charter("Juniper Bakery", "Fresh bread for Nakuru every morning.", "KES");
    site.commit(&charter, &[(Role::Owner, OWNER)]).await?;
    let (steward, _) = site.back_office(
        &mode,
        "steward",
        &charter,
        vec![
            call("interview_record", json!({ "text": "Juniper Bakery's phone number is +254 700 000 001", "quote": "Our phone number is +254 700 000 001" })),
            call("interview_record", json!({ "text": "Juniper Bakery opens at seven on weekdays", "quote": "we open at seven on weekdays" })),
            ScriptedResponse::text("Thank you, both confirmed."),
        ],
        |b| {
            b.instruction(
                "You are the Steward onboarding Juniper Bakery. Discovery found its listing, \
                 reviews and social profile; nothing it found is confirmed. Record every fact the \
                 owner confirms with interview_record, quoting the owner's exact words, then reply \
                 briefly.",
            )
            .mode(HarnessMode::Ask)
        },
    )?;
    let owner = user(OWNER);
    let session = steward.open_session(&owner, None).await?;
    let events = collect(steward.send(&owner, &session, OWNER_SAYS).await?).await?;
    if mode.is_live() {
        trace(&events);
    }
    let known = known_facts(site.facts.as_ref(), &scope).await?;
    checks.require(
        "the owner's confirmations are Known",
        known.iter().any(|f| f.text.contains("700 000 001")),
        known.iter().map(|f| f.text.as_str()).collect::<Vec<_>>().join("; "),
    )?;

    // A page claiming an Assumed fact is held by the claims check (A-I18).
    let address = site
        .facts
        .list(FactFilter::recall(&scope))
        .await?
        .into_iter()
        .find(|f| f.text.starts_with("address:"))
        .ok_or_else(|| anyhow!("the address was found"))?;
    let mut claim = SitePlan::new(&charter, "Juniper Bakery")
        .items(&charter, &[], &["KE".to_string()])
        .into_iter()
        .find(|item| item.slug == "contact")
        .ok_or_else(|| anyhow!("a contact page"))?;
    claim.body = Some(Markdown("Find us at 4 Kenyatta Avenue.\n".into()));
    claim.sources = vec![FactRef(address.id.clone())];
    let draft = site
        .runtime
        .pipeline()
        .draft(site.steward_change(vec![claim.clone()], "onboarding"))
        .await?;
    let verdict = site.runtime.pipeline().check(&draft).await?;
    checks.check(
        "a page citing an unconfirmed fact is held",
        verdict.held_items().contains(&claim.id),
        verdict.summary(),
    );

    // The owner approves Wanjiru's review, and the anonymous one; only an attributed and
    // approved review is quoted.
    let mut reviews: Vec<ReviewQuote> =
        discovered.inventory.as_ref().map(|i| i.reviews.clone()).unwrap_or_default();
    let quotable_before = reviews.iter().filter(|r| r.usable()).count();
    for (n, review) in reviews.iter_mut().enumerate() {
        review.approved_by = Some(format!("dec-review-{n}"));
    }
    let quoted: Vec<&ReviewQuote> = reviews.iter().filter(|r| r.usable()).collect();
    checks.check(
        "no review is quotable before the owner approves it; an anonymous one never is",
        quotable_before == 0 && quoted.len() == 1 && reviews.len() == 2,
        format!("{} found, {} quotable after approval", reviews.len(), quoted.len()),
    );

    // Release 1: the plan's pages from Known facts, the reviews page quoting what may be quoted.
    let mut items: Vec<ContentItem> =
        SitePlan::new(&charter, "Juniper Bakery").items(&charter, &known, &["KE".to_string()]);
    if let Some(page) = items.iter_mut().find(|item| item.slug == "reviews") {
        let quotes: String = quoted
            .iter()
            .map(|r| {
                format!("> {}\n>\n> — {}\n\n", r.text, r.author.as_deref().unwrap_or_default())
            })
            .collect();
        page.body = Some(Markdown(format!("What people say, with their permission.\n\n{quotes}")));
    }
    let outcome = site.runtime.pipeline().run(site.steward_change(items, "onboarding")).await?;
    checks.require(
        "release 1 is published",
        matches!(outcome, adk_site::pipeline::PipelineOutcome::Published(_)),
        match &outcome {
            adk_site::pipeline::PipelineOutcome::Published(p) => {
                format!("release {}", p.staged.manifest.seq)
            }
            other => format!("{other:?}").chars().take(160).collect(),
        },
    )?;
    let release = verify_served(&site.host, "juniper.example", &site.release_key).await?;
    let page = |path: &str| {
        let host = site.host.clone();
        let path = path.to_string();
        async move {
            host.fetch(&path)
                .await
                .ok()
                .flatten()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default()
        }
    };
    let about = page("/about/index.html").await;
    let reviews_page = page("/reviews/index.html").await;
    let mut all_pages = String::new();
    for path in ["/index.html", "/about/index.html", "/contact/index.html", "/reviews/index.html"] {
        all_pages.push_str(&page(path).await);
    }
    checks.check(
        "only confirmed facts appear on pages",
        about.contains("700 000 001")
            && !all_pages.contains("Kenyatta Avenue")
            && !all_pages.contains("4.8"),
        format!("release {release}: the phone shows; the address and the rating do not"),
    );
    checks.gap(
        "nothing in the build or the launch gate places or polices review quotes: release 1 \
         quotes reviews only because the scenario filters them with `ReviewQuote::usable`; a \
         Steward draft quoting an anonymous or unapproved review would pass every check",
    );
    checks.check(
        "review quotes appear only with attribution and the owner's approval",
        reviews_page.contains("still warm at seven")
            && reviews_page.contains("Wanjiru K.")
            && !all_pages.contains("sold out by noon"),
        "Wanjiru K.'s approved quote shows; the anonymous one does not",
    );
    Ok(checks)
}
