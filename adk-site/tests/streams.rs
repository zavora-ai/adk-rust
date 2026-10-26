//! The stream lane (spec 3.5 §7.8.3 "Three lanes", A-I26): entries signed by the edge key
//! in the AWP 2.0 proof format, served at once, folded into signed releases every five
//! minutes and at the close.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use adk_awp::signing::jws::{self, Ed25519DocumentSigner};
use adk_core::audit_log::InMemoryAuditLogStore;
use adk_core::{Clock, ManualClock};
use adk_site::content::{
    ContentStore, InMemoryContentHistory, InMemoryContentStore, SiteConfig, SiteProject,
};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::{DirectoryHost, SiteHost};
use adk_site::lanes::KindLanes;
use adk_site::live::InMemoryLiveStore;
use adk_site::log::SiteLog;
use adk_site::pipeline::PublishingPipeline;
use adk_site::signer::Ed25519ReleaseSigner;
use adk_site::stream::{Ed25519EntrySigner, EntrySigner, StreamLane, sign_entry, verify_entry};
use adk_site::updates::UpdateEngine;
use adk_site::{Result, SiteError};
use awp_types::{
    ContentId, ContentItem, ContentKind, ContentState, Markdown, Proof, ReleaseSource, StreamEntry,
};
use chrono::{TimeZone, Utc};
use serde_json::json;

fn story(state: ContentState) -> ContentItem {
    ContentItem {
        id: "articles/count/en".into(),
        kind: ContentKind::Article,
        slug: "count".into(),
        language: "en".parse().unwrap(),
        fields: BTreeMap::from([
            ("title".to_string(), json!("The count, live")),
            ("publishedAt".to_string(), json!("2026-11-03T18:00:00Z")),
        ]),
        volatile: Vec::new(),
        body: Some(Markdown("Polls are closed.".into())),
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state,
    }
}

#[tokio::test]
async fn entries_carry_the_awp_proof_both_ways() {
    let fixture: StreamEntry = serde_json::from_str(include_str!(
        "../../adk-awp/tests/fixtures/awp-2.0/stream-entry.json"
    ))
    .unwrap();
    assert_eq!(fixture.n, 17, "the edge's wire format parses");

    // Signed here, verified by the edge's verifier.
    let signer = Ed25519EntrySigner::from_seed("edge-receipt-2026-11", [4; 32]).unwrap();
    let mut entry = StreamEntry { proof: Proof::eddsa("", ""), ..fixture.clone() };
    entry.proof = sign_entry(&entry, &signer).await.unwrap();
    jws::verify(&entry, &entry.proof, &signer.public_key()).unwrap();

    // Signed by the edge's signer, verified here.
    let edge = Ed25519DocumentSigner::from_seed("edge-receipt-2026-11", [4; 32]).unwrap();
    let mut by_edge = fixture;
    by_edge.proof = jws::sign(&by_edge, &edge).await.unwrap();
    verify_entry(&by_edge, &signer.public_key()).unwrap();
    assert_eq!(by_edge.proof, entry.proof, "the same key gives the same detached JWS");
    by_edge.markdown = Markdown("Altered.".into());
    assert!(verify_entry(&by_edge, &signer.public_key()).is_err());
}

#[tokio::test]
async fn a_live_story_folds_every_five_minutes_and_at_the_close() -> Result<()> {
    let clock = Arc::new(ManualClock::new(Utc.with_ymd_and_hms(2026, 11, 3, 20, 0, 0).unwrap()));
    let (host_dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let host = Arc::new(DirectoryHost::new(host_dir.path()).materialise(false));
    let signer = Arc::new(Ed25519ReleaseSigner::from_seed("news.example", [2; 32])?);
    let log = Arc::new(
        SiteLog::open_with_clock(
            Arc::new(InMemoryAuditLogStore::new()),
            "news.example",
            signer,
            clock.clone(),
        )
        .await?,
    );
    let mut config = SiteConfig::new("News", "news.example", vec!["en".parse().unwrap()]);
    config.lanes.insert(
        ContentKind::Article,
        KindLanes { stream: vec!["entries".into()], ..KindLanes::default() },
    );
    let store = Arc::new(InMemoryContentStore::default());
    let pipeline = Arc::new(
        PublishingPipeline::builder(
            SiteProject::new(config, Vec::new(), clock.now()),
            store.clone(),
            Arc::new(BuiltinGenerator),
            host.clone(),
            log,
            work.path(),
        )
        .history(Arc::new(InMemoryContentHistory::default()))
        .clock(clock.clone())
        .build(),
    );
    let edge = Arc::new(Ed25519EntrySigner::from_seed("edge-receipt", [6; 32])?);
    let engine = UpdateEngine::builder(pipeline.clone(), Arc::new(InMemoryLiveStore::default()))
        .streams(Arc::new(StreamLane::new(edge.clone())))
        .build()?;
    let id = ContentId::from("articles/count/en");

    // A story opens its stream only once it is published.
    store.put(&story(ContentState::InReview)).await?;
    assert!(matches!(engine.open_stream(&id, "entries").await, Err(SiteError::Pipeline(_))));
    pipeline
        .publish_batch(
            vec![story(ContentState::Published)],
            Vec::new(),
            ReleaseSource::Onboarding,
            "urn:person:editor",
        )
        .await?;
    assert!(matches!(engine.open_stream(&id, "body").await, Err(SiteError::InvalidConfig(_))));
    engine.open_stream(&id, "entries").await?;

    // Twelve entries, one every 30 s: served at once, folded at five minutes.
    let mut released = Vec::new();
    for n in 1..=12u64 {
        let entry = engine.append_entry(&id, &format!("District {n} reports."), None).await?;
        assert_eq!(engine.stream_since(&id, n - 1).await?, vec![entry]);
        clock.advance(Duration::from_secs(30));
        released.extend(engine.tick().await?);
    }
    let correction = engine.append_entry(&id, "District 3's count was 1,204.", Some(3)).await?;
    assert_eq!(correction.corrects, Some(3));
    assert!(engine.append_entry(&id, "Bad.", Some(99)).await.is_err());
    engine.close_stream(&id).await?;
    assert!(engine.append_entry(&id, "After the close.", None).await.is_err());
    clock.advance(Duration::from_secs(1));
    released.extend(engine.tick().await?);
    clock.advance(Duration::from_secs(10));
    released.extend(engine.tick().await?);

    let folds: Vec<_> = released.iter().filter(|r| r.release_seq.is_some()).collect();
    assert_eq!(folds.len(), 2, "one fold at five minutes and one at the close: {folds:?}");
    let status = engine.streams().await.remove(0);
    assert_eq!((status.entries, status.folded, status.folds), (13, 13, 2));
    let item = engine.pipeline().content_store().get(&id).await?.unwrap();
    let entries: Vec<StreamEntry> = serde_json::from_value(item.fields["entries"].clone()).unwrap();
    assert_eq!(entries.len(), 13, "every entry ends up in a signed release");
    for entry in &entries {
        verify_entry(entry, &edge.public_key())?;
    }
    let page = String::from_utf8(host.fetch("/articles/count/").await?.unwrap().to_vec()).unwrap();
    assert!(page.contains(r#"data-awp-stream="articles/count/en""#));
    assert!(page.contains("District 12 reports."));
    assert!(page.contains("corrects #3"));
    let twin =
        String::from_utf8(host.fetch("/articles/count/index.md").await?.unwrap().to_vec()).unwrap();
    assert!(twin.contains("District 1 reports."));
    Ok(())
}
