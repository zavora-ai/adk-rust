//! S6 — identity, not claims (spec 3.5 §5 S6, §7.3, §7.4, AWP 2.0 §6).
//!
//! Three callers reach a launched shop's agent edge: an unsigned client claiming `GPTBot` with
//! `X-AWP-Channel: agent`, a Web Bot Auth-signed buyer agent, and a partner with an OAuth
//! token.
//!
//! Pass condition: trust `public`, `known`, `partner`; each sees only the capabilities its tier
//! allows; limits key on the verified identity.

use std::collections::BTreeSet;

use adk_server::awp::EdgeLimits;
use anyhow::Result;
use awp_types::{RateClass, SiteKind, TrustTier};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};

use crate::checks::Checks;
use crate::fixture::edge::{EdgeOptions, EdgeSite, edge_with};
use crate::fixture::*;
use crate::live::Mode;

const DOMAIN: &str = "juniper.example";

fn claiming(mut request: Request<Body>) -> Request<Body> {
    let headers = request.headers_mut();
    headers.insert(
        header::USER_AGENT,
        "GPTBot/1.2 (+https://openai.com/gptbot)".parse().expect("a header"),
    );
    headers.insert("x-awp-channel", "agent".parse().expect("a header"));
    request
}

fn set(manifest: &Value) -> BTreeSet<String> {
    EdgeSite::names(manifest).into_iter().collect()
}

/// Runs S6.
pub async fn run(_mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let site = Site::new(SiteKind::Ecommerce, "Juniper Bakery", DOMAIN, &["en"]).await?;
    site.launch("Juniper Bakery", "KES", Vec::new()).await?;
    let limits = EdgeLimits::default()
        .tier_default(TrustTier::Known, RateClass { requests_per_minute: 3, calls_per_day: 100 })
        .tier_default(TrustTier::Public, RateClass { requests_per_minute: 6, calls_per_day: 100 });
    let edge =
        edge_with(&site, DOMAIN, EdgeOptions { limits: Some(limits), ..EdgeOptions::default() })
            .await?;

    let (status, _, public) = edge.send(claiming(edge.get("/awp/v2/manifest"))).await;
    let (_, _, known) = edge.send(edge.signed(edge.get("/awp/v2/manifest"))).await;
    let (_, _, partner) = edge.send(edge.partner(edge.get("/awp/v2/manifest"))).await;
    checks.check(
        "trust public, known, partner — from credentials, never from claims",
        status == StatusCode::OK
            && public["tier"] == "public"
            && known["tier"] == "known"
            && partner["tier"] == "partner",
        format!(
            "the GPTBot claim is {}, the signed agent {}, the OAuth partner {}",
            public["tier"], known["tier"], partner["tier"]
        ),
    );
    let (public, known, partner) = (set(&public), set(&known), set(&partner));
    let declared: Vec<_> = site.template.capabilities();
    let tier_of = |name: &str| declared.iter().find(|c| c.name == name).map(|c| c.min_trust);
    let within = |names: &BTreeSet<String>, tier: TrustTier| {
        names.iter().all(|n| tier_of(n).is_some_and(|t| t <= tier))
            && declared.iter().filter(|c| c.min_trust <= tier).all(|c| names.contains(&c.name))
    };
    checks.check(
        "each sees only the capabilities its tier allows",
        within(&public, TrustTier::Public)
            && within(&known, TrustTier::Known)
            && within(&partner, TrustTier::Partner)
            && public.is_subset(&known)
            && known.len() > public.len(),
        format!(
            "public {}, known +{:?}, partner +{:?}",
            public.len(),
            known.difference(&public).collect::<Vec<_>>(),
            partner.difference(&known).collect::<Vec<_>>()
        ),
    );
    let quote = json!({ "items": [{ "sku": "none", "quantity": 1 }] });
    let (hidden, _, body) =
        edge.send(claiming(edge.call("get_quote", "q-claim", None, &quote))).await;
    checks.check(
        "a capability above the caller's tier answers as one that does not exist",
        hidden == StatusCode::NOT_FOUND && body["error"]["code"] == "capability_not_found",
        format!("{hidden} {}", body["error"]["code"]),
    );
    let (_, _, card) = edge.send(claiming(edge.get("/.well-known/agent-card.json"))).await;
    let skills: BTreeSet<String> = card["skills"]
        .as_array()
        .map(|s| s.iter().filter_map(|k| k["id"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    checks.check(
        "the Agent Card lists only the skills the caller's tier sees",
        skills == public,
        format!("{} skills for the claimer", skills.len()),
    );
    let mut forged = edge.signed(edge.get("/awp/v2/manifest"));
    *forged.uri_mut() = "https://other.example/awp/v2/manifest".parse()?;
    let (failed, _, _) = edge.send(forged).await;
    checks.check(
        "a credential that fails its check is refused, never treated as public",
        failed == StatusCode::UNAUTHORIZED,
        format!("{failed}"),
    );

    let (listed, _, _) =
        edge.send(edge.signed(edge.call("list_products", "l-1", None, &json!({})))).await;
    checks.check(
        "the signed agent calls a capability",
        listed == StatusCode::OK,
        format!("{listed}"),
    );

    // Limits key on the verified identity: the signed agent's window is its own; an unsigned
    // caller claiming to be the same agent has the public window.
    let mut statuses = Vec::new();
    for _ in 0..4 {
        statuses.push(edge.send(edge.signed(edge.get("/awp/v2/manifest"))).await.0);
    }
    let (limited, headers, body) = edge.send(edge.signed(edge.get("/awp/v2/manifest"))).await;
    let mut claim = edge.get("/awp/v2/manifest");
    claim
        .headers_mut()
        .insert(header::USER_AGENT, "BuyerBot/1.0 (+https://buyer.example)".parse()?);
    let (claimer, _, _) = edge.send(claim).await;
    checks.check(
        "limits key on the verified identity",
        limited == StatusCode::TOO_MANY_REQUESTS
            && body["error"]["code"] == "rate_limit_exceeded"
            && headers
                .get("ratelimit-policy")
                .is_some_and(|v| v.to_str().unwrap_or_default().contains("tier-known"))
            && claimer == StatusCode::OK,
        format!("signed agent {statuses:?} then {limited}; the claimer {claimer}"),
    );
    let records = edge.audit.records();
    checks.check(
        "the edge's records name the verified requester",
        records.iter().any(|r| r.ext.values().any(|v| v.as_str() == Some(edge.buyer_id.as_str()))),
        format!("{} edge records", records.len()),
    );
    Ok(checks)
}
