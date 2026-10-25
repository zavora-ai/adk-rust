//! The content model (spec 3.5 §7.8.2): both stores and both histories run their
//! conformance suites; the repository layout loads; schedules are data.

use std::collections::BTreeMap;

use adk_site::content::{
    FileContentHistory, FsContentStore, InMemoryContentHistory, InMemoryContentStore, SiteConfig,
    SiteProject, content_history_conformance, content_store_conformance,
};
use adk_site::pipeline::{ScheduledActionKind, due_actions, next_wake, schedule_actions};
use awp_types::{ContentItem, ContentKind, ContentState, Markdown, Schedule};
use chrono::{TimeZone, Utc};

fn sample() -> ContentItem {
    ContentItem {
        id: "products/panel-400/en".into(),
        kind: ContentKind::Product,
        slug: "panel-400".into(),
        language: "en".parse().unwrap(),
        fields: BTreeMap::from([
            ("title".to_string(), serde_json::json!("Panel 400 W")),
            ("price".to_string(), serde_json::json!(25000.5)),
            ("tags".to_string(), serde_json::json!(["solar", "panels"])),
        ]),
        volatile: vec!["stock".into()],
        body: Some(Markdown("Rated at 400 W.\n\n- one\n- two".into())),
        media: vec![],
        sources: vec![awp_types::FactRef("fact-1".into())],
        schedule: Some(Schedule {
            publish_at: Some(Utc.with_ymd_and_hms(2026, 11, 1, 9, 0, 0).unwrap()),
            ..Schedule::default()
        }),
        state: ContentState::Scheduled,
    }
}

#[tokio::test]
async fn both_stores_pass_the_store_conformance_suite() {
    content_store_conformance(&InMemoryContentStore::default(), sample()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = FsContentStore::new(dir.path());
    content_store_conformance(&store, sample()).await;
    assert!(
        dir.path().join("content/products/panel-400/en.md").exists(),
        "items live at content/<kind>/<slug>/<language>.md"
    );
}

#[tokio::test]
async fn both_histories_pass_the_history_conformance_suite() {
    content_history_conformance(&InMemoryContentHistory::default(), sample()).await;
    let dir = tempfile::tempdir().unwrap();
    content_history_conformance(&FileContentHistory::new(dir.path()), sample()).await;
}

#[tokio::test]
async fn a_repository_loads_and_reports_its_problems() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("site.toml"),
        "name = \"Shop\"\ndomain = \"shop.example\"\nlanguages = [\"en\"]\n",
    )
    .unwrap();
    let store = FsContentStore::new(dir.path());
    use adk_site::content::ContentStore;
    store.put(&sample()).await.unwrap();
    let mut french = sample();
    french.id = "products/panel-400/fr".into();
    french.language = "fr".parse().unwrap();
    french.media.push(awp_types::MediaRef {
        path: "content/products/panel-400/missing.png".into(),
        content_type: "image/png".into(),
        sha256: None,
        alt: None,
        alt_confirmed: false,
        generated: false,
    });
    store.put(&french).await.unwrap();
    let project = SiteProject::load(dir.path(), Utc::now()).await.unwrap();
    assert_eq!(project.items.len(), 2);
    let messages: Vec<String> = project.issues().into_iter().map(|i| i.message).collect();
    assert!(messages.iter().any(|m| m.contains("language 'fr'")), "{messages:?}");
    assert!(messages.iter().any(|m| m.contains("media")), "{messages:?}");

    assert!(
        SiteConfig::from_toml(
            "name = \"x\"\ndomain = \"https://x.example/\"\nlanguages = [\"en\"]"
        )
        .is_err()
    );
    assert!(SiteConfig::from_toml("name = \"x\"\ndomain = \"x.example\"\nlanguages = []").is_err());
}

#[test]
fn schedules_are_data() {
    let mut item = sample();
    let publish = Utc.with_ymd_and_hms(2026, 11, 1, 9, 0, 0).unwrap();
    let expires = Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap();
    item.schedule.as_mut().unwrap().expires_at = Some(expires);
    let actions = schedule_actions(std::slice::from_ref(&item));
    assert_eq!(
        actions.iter().map(|a| a.kind).collect::<Vec<_>>(),
        vec![ScheduledActionKind::Publish, ScheduledActionKind::Expire]
    );
    let before = Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap();
    assert_eq!(next_wake(std::slice::from_ref(&item), before), Some(publish));
    assert!(due_actions(std::slice::from_ref(&item), None, before).is_empty());
    assert_eq!(due_actions(std::slice::from_ref(&item), Some(before), publish).len(), 1);
    assert_eq!(next_wake(std::slice::from_ref(&item), publish), Some(expires));
    item.state = ContentState::Draft;
    assert!(schedule_actions(&[item]).is_empty(), "drafts have no schedule yet");
}
