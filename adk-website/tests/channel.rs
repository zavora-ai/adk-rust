//! The owner's chat channel (spec 3.5 §5 S18, §7.8.2 "Media", A-I24): photos from the paired
//! phone join the Steward's draft, the owner confirms their alt text from the thread, and the
//! confirmation reaches the item once it is in the content store; a stranger adds nothing.

mod common;

use std::sync::Arc;

use adk_harness::website::{AuthoringRouter, back_office_tools};
use adk_kernel::channels::{
    ChannelBinding, ChannelBindingStore, ChannelInbound, InMemoryChannelBindingStore,
};
use adk_site::content::ContentStore;
use adk_tool::SimpleToolContext;
use adk_website::channel::{ChannelPhoto, OwnerChannel};
use awp_types::{ContentId, SiteKind};
use base64::Engine as _;
use common::*;
use serde_json::json;

/// A 1×1 PNG.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

fn photo(n: usize) -> ChannelPhoto {
    ChannelPhoto {
        name: format!("seeded-rye-{n}.png"),
        bytes: base64::engine::general_purpose::STANDARD.decode(PNG).unwrap().into(),
        caption: (n == 0).then(|| "Seeded rye on the counter".to_string()),
    }
}

#[tokio::test]
async fn photos_from_the_paired_phone_reach_the_draft_and_the_thread_confirms_their_alt_text() {
    let site = Site::new(SiteKind::Ecommerce, "Juniper Bakery", "juniper.example", &["en"]).await;
    let bindings = Arc::new(InMemoryChannelBindingStore::new());
    bindings
        .put_binding(ChannelBinding::new(
            "whatsapp",
            "+254700000001",
            "wa-1",
            OWNER,
            chrono::Utc::now(),
        ))
        .await
        .unwrap();
    let channel = OwnerChannel::new(site.runtime.clone(), AuthoringRouter::new(bindings)).unwrap();

    // The Steward drafted the product during the owner's turn.
    let draft_tool =
        back_office_tools(&site.runtime).into_iter().find(|t| t.name() == "content_draft").unwrap();
    let drafted = draft_tool
        .execute(
            Arc::new(SimpleToolContext::new("steward")),
            json!({ "kind": "product", "slug": "seeded-rye", "language": "en",
                    "fields": { "title": "Seeded rye", "price": 520, "currency": "KES", "goal": "sales" },
                    "body": "Rye with seeds." }),
        )
        .await
        .unwrap();
    let draft = drafted["draft"].as_str().unwrap().to_string();
    let item = ContentId("products/seeded-rye/en".into());

    // A stranger's photos reach nothing (A-I24).
    let stranger = ChannelInbound::text("whatsapp", "+254799999999", "wa-9", "m-0", "photos");
    let refused = channel.attach(&stranger, &draft, &item, "Seeded rye", vec![photo(0)]).await;
    assert!(refused.unwrap_err().to_string().contains("A-I24"));

    let message = ChannelInbound::text("whatsapp", "+254700000001", "wa-1", "m-1", "three photos");
    let attached = channel
        .attach(&message, &draft, &item, "Seeded rye", (0..3).map(photo).collect())
        .await
        .unwrap();
    assert_eq!(attached.photos.len(), 3);
    assert_eq!(attached.photos[0].alt, "Seeded rye on the counter", "the caption drafts it");
    assert_eq!(attached.photos[2].alt, "Seeded rye, photo 3");
    assert!(attached.reply.contains("Reply yes"), "{}", attached.reply);

    // The owner changes the second description and confirms the rest from the thread.
    let changed = ChannelInbound::text(
        "whatsapp",
        "+254700000001",
        "wa-1",
        "m-2",
        "2: A sliced seeded rye loaf",
    );
    assert_eq!(
        channel.reply(&changed).await.unwrap().as_deref(),
        Some("Photo 2 now reads: A sliced seeded rye loaf")
    );
    let yes = ChannelInbound::text("whatsapp", "+254700000001", "wa-1", "m-3", "Yes");
    assert!(channel.reply(&yes).await.unwrap().is_some());
    let other = ChannelInbound::text("whatsapp", "+254700000001", "wa-1", "m-4", "Make it 09:00");
    assert_eq!(channel.reply(&other).await.unwrap(), None, "everything else goes to the Steward");
    assert_eq!(channel.waiting("wa-1").await.len(), 3, "the draft is not in the store yet");

    // Once the item is in the store, as a scheduled draft is, the confirmations apply.
    let mut stored = awp_types::ContentItem {
        id: item.clone(),
        kind: awp_types::ContentKind::Product,
        slug: "seeded-rye".into(),
        language: "en".parse().unwrap(),
        fields: Default::default(),
        volatile: Vec::new(),
        body: None,
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: awp_types::ContentState::Scheduled,
    };
    for photo in &attached.photos {
        stored.media.push(awp_types::MediaRef {
            path: photo.path.clone(),
            content_type: "image/png".into(),
            sha256: None,
            alt: Some(photo.alt.clone()),
            alt_confirmed: false,
            generated: false,
        });
    }
    site.store.put(&stored).await.unwrap();
    assert_eq!(channel.settle().await.unwrap(), 3);
    let confirmed = site.store.get(&item).await.unwrap().unwrap();
    assert!(confirmed.media.iter().all(|m| m.alt_confirmed), "{:?}", confirmed.media);
    assert_eq!(confirmed.media[1].alt.as_deref(), Some("A sliced seeded rye loaf"));
    assert!(channel.waiting("wa-1").await.is_empty());
}

/// The Steward scheduled the draft before the photos arrived (spec 3.5 S18): they join the
/// scheduled item in the content store, which is not live yet; a live item is left alone.
#[tokio::test]
async fn photos_that_arrive_after_scheduling_join_the_scheduled_item() {
    let site = Site::new(SiteKind::Ecommerce, "Juniper Bakery", "juniper.example", &["en"]).await;
    let bindings = Arc::new(InMemoryChannelBindingStore::new());
    bindings
        .put_binding(ChannelBinding::new(
            "whatsapp",
            "+254700000001",
            "wa-1",
            OWNER,
            chrono::Utc::now(),
        ))
        .await
        .unwrap();
    let channel = OwnerChannel::new(site.runtime.clone(), AuthoringRouter::new(bindings)).unwrap();
    let item = ContentId("products/seeded-rye/en".into());
    let mut scheduled = awp_types::ContentItem {
        id: item.clone(),
        kind: awp_types::ContentKind::Product,
        slug: "seeded-rye".into(),
        language: "en".parse().unwrap(),
        fields: Default::default(),
        volatile: Vec::new(),
        body: None,
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: awp_types::ContentState::Scheduled,
    };
    site.store.put(&scheduled).await.unwrap();
    let message = ChannelInbound::text("whatsapp", "+254700000001", "wa-1", "m-1", "photos");
    let attached = channel
        .attach(&message, "draft-gone", &item, "Seeded rye", (0..2).map(photo).collect())
        .await
        .unwrap();
    assert_eq!(attached.photos.len(), 2);
    let stored = site.store.get(&item).await.unwrap().unwrap();
    assert_eq!(stored.media.len(), 2);
    for media in &stored.media {
        assert!(site.store.read_media(&media.path).await.unwrap().is_some(), "{}", media.path);
    }

    scheduled.id = ContentId("products/rye-loaf/en".into());
    scheduled.slug = "rye-loaf".into();
    scheduled.state = awp_types::ContentState::Published;
    site.store.put(&scheduled).await.unwrap();
    let live =
        channel.attach(&message, "draft-gone", &scheduled.id, "Rye loaf", vec![photo(0)]).await;
    assert!(live.is_err(), "a live item changes through a release, not the channel");
}
