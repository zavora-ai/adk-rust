//! Visual memory (spec 3.7 §7.15) over adk-rag's in-memory vector store: the shared suite,
//! a space re-recorded under an open memory, the kill switch, and the S16 week corpus
//! indexed and erased through a scripted embedder.

mod memory_suite;

use std::sync::Arc;

use adk_core::GovernanceControl;
use adk_rag::{Field, Filter, InMemoryVectorStore, Scope, VectorStore};
use adk_vision::memory::fixtures::{WeekCorpus, WeekSpec};
use adk_vision::memory::{SPACE_SCOPE, VisualItem, VisualMemory, VisualQuery};
use adk_vision::{MediaScope, Provenance, VisionCx, VisionError};
use memory_suite::{COLLECTION, Fixture, run, runtime};

#[tokio::test]
async fn the_suite_passes_on_the_in_memory_store() {
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new());
    run(|| {
        let store = store.clone();
        async move { store }
    })
    .await;
}

#[tokio::test]
async fn a_collection_re_recorded_in_another_space_refuses_the_old_memory() {
    let fx = Fixture::new().await;
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new());
    let old = fx.memory(store.clone(), runtime("1", 8)).await;
    // Another writer empties the collection and rebuilds it with a new embedder.
    store.purge_scope(COLLECTION, &Scope::new(SPACE_SCOPE).unwrap()).await.unwrap();
    fx.memory(store.clone(), runtime("2", 8)).await;

    let (image, pixels) = fx.image([200, 10, 10], 1).await;
    let item = VisualItem::new(
        image,
        pixels,
        "a frame",
        "camera-1",
        memory_suite::at("2026-11-17T08:00:00Z"),
    );
    let scope = Scope::new("line-a").unwrap();
    let err = old.index(&fx.cx, &scope, &[item]).await.unwrap_err();
    assert!(matches!(err, VisionError::SpaceMismatch { .. }), "{err}");
    let err = old.search_text(&fx.cx, &scope, "red", &VisualQuery::top(1)).await.unwrap_err();
    assert!(matches!(err, VisionError::SpaceMismatch { .. }), "{err}");
    assert_eq!(store.count(COLLECTION, &scope, None).await.unwrap(), 0, "nothing was mixed in");
}

#[tokio::test]
async fn a_frozen_kill_switch_refuses_indexing_and_search_but_not_erasure() {
    let fx = Fixture::new().await;
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new());
    let memory = fx.memory(store.clone(), runtime("1", 8)).await;
    let scope = Scope::new("line-a").unwrap();
    let (image, pixels) = fx.image([200, 10, 10], 1).await;
    let item = VisualItem::new(
        image,
        pixels,
        "a frame",
        "camera-1",
        memory_suite::at("2026-11-17T08:00:00Z"),
    );
    memory.index(&fx.cx, &scope, std::slice::from_ref(&item)).await.unwrap();

    let control = GovernanceControl::new();
    let frozen = fx.cx.clone().with_governance(control.clone());
    control.freeze("incident 7");
    assert!(memory.index(&frozen, &scope, &[item]).await.is_err());
    assert!(memory.search_text(&frozen, &scope, "red", &VisualQuery::top(1)).await.is_err());
    let erased = memory.erase_source(&frozen, &scope, "camera-1").await.unwrap();
    assert_eq!(erased.chunks, 1);
}

#[tokio::test]
async fn pixels_of_another_image_are_refused() {
    let fx = Fixture::new().await;
    let memory = fx
        .memory(Arc::new(InMemoryVectorStore::new()) as Arc<dyn VectorStore>, runtime("1", 8))
        .await;
    let (image, _) = fx.image([200, 10, 10], 1).await;
    let (_, other) = fx.image([10, 10, 200], 2).await;
    let item = VisualItem::new(
        image,
        other,
        "a frame",
        "camera-1",
        memory_suite::at("2026-11-17T08:00:00Z"),
    );
    let err = memory.index(&fx.cx, &Scope::new("a").unwrap(), &[item]).await.unwrap_err();
    assert!(matches!(err, VisionError::InvalidInput(_)), "{err}");
}

#[tokio::test]
async fn the_week_corpus_indexes_by_day_and_erases_one_camera() {
    let fx = Fixture::new().await;
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new());
    let memory = fx.memory(store.clone(), runtime("1", 8)).await;
    let corpus = WeekCorpus::new(WeekSpec::default());
    let scope = Scope::from_parts(["inspector", "plant-7"]).unwrap();
    let media = MediaScope::new("plant", "u1", "s1");
    let mut items = Vec::new();
    for week in corpus.items() {
        let (image, pixels) =
            fx.media.put_pixels(&media, corpus.render(week), Provenance::Upload).await.unwrap();
        items.push(
            VisualItem::new(
                image,
                pixels,
                week.caption.clone(),
                week.source.clone(),
                week.captured_at,
            )
            .with_kind(week.kind)
            .with_run(format!("run-{}", week.captured_at.date_naive())),
        );
    }
    memory.index(&VisionCx::new(), &scope, &items).await.unwrap();
    assert_eq!(store.count(COLLECTION, &scope, None).await.unwrap(), corpus.items().len() as u64);
    let tuesday = corpus.day(chrono::Weekday::Tue);
    let hits = memory
        .search_text(&fx.cx, &scope, "anything", &VisualQuery::top(100).captured_on([tuesday]))
        .await
        .unwrap()
        .value;
    assert_eq!(hits.len(), 15, "twelve frames and three pages a day");
    assert!(hits.iter().all(|h| h.run.as_deref() == Some(&*format!("run-{tuesday}"))));

    let camera = Filter::eq(Field::DocumentId, "conveyor-2");
    let before = store.count(COLLECTION, &scope, Some(&camera)).await.unwrap();
    assert_eq!(before, 42);
    let erased = memory.erase_source(&fx.cx, &scope, "conveyor-2").await.unwrap();
    assert_eq!(erased.chunks, before);
    assert_eq!(erased.artifacts.len() as u64, before);
    assert_eq!(store.count(COLLECTION, &scope, Some(&camera)).await.unwrap(), 0);
    assert_eq!(
        store.count(COLLECTION, &scope, None).await.unwrap(),
        corpus.items().len() as u64 - before
    );
    let _ = VisualMemory::builder(runtime("1", 8), store, COLLECTION).open().await.unwrap();
}
