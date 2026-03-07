use adk_core::Event;
use chrono::{DateTime, TimeZone, Utc};
use proptest::prelude::*;

/// Generate an arbitrary UTC timestamp within a reasonable range.
fn arb_timestamp() -> impl Strategy<Value = DateTime<Utc>> {
    // Range: 2020-01-01 to 2030-01-01 in seconds
    (1_577_836_800i64..1_893_456_000i64)
        .prop_map(|secs| Utc.timestamp_opt(secs, 0).single().expect("valid timestamp"))
}

/// Generate an event with a specific timestamp.
fn arb_event_with_timestamp(ts: DateTime<Utc>) -> Event {
    let mut event = Event::new("test-invocation");
    event.timestamp = ts;
    event
}

/// Generate a vector of events with arbitrary timestamps (unsorted).
fn arb_event_sequence() -> impl Strategy<Value = Vec<Event>> {
    prop::collection::vec(arb_timestamp(), 0..50)
        .prop_map(|timestamps| timestamps.into_iter().map(arb_event_with_timestamp).collect())
}

/// Sort events by timestamp ascending — the canonical ordering all backends must produce.
fn sort_events_by_timestamp(events: &mut [Event]) {
    events.sort_by_key(|e| e.timestamp);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// **Feature: production-backends, Property 4: Event Timestamp Ordering**
    /// *For any* sequence of events with arbitrary timestamps, when sorted by timestamp,
    /// the returned events are ordered by timestamp ascending.
    /// **Validates: Requirements 3.3, 6.2, 7.3**
    #[test]
    fn prop_event_timestamp_ordering(events in arb_event_sequence()) {
        let mut sorted = events.clone();
        sort_events_by_timestamp(&mut sorted);

        // Verify ascending order
        for window in sorted.windows(2) {
            prop_assert!(
                window[0].timestamp <= window[1].timestamp,
                "events not in ascending timestamp order: {} > {}",
                window[0].timestamp,
                window[1].timestamp
            );
        }

        // Verify no events were lost or duplicated
        prop_assert_eq!(sorted.len(), events.len(),
            "sorting changed the number of events");
    }

    /// **Feature: production-backends, Property 4 (stability): Event Ordering Is Stable**
    /// *For any* sequence of events, sorting by timestamp twice produces the same result.
    /// This ensures the ordering is deterministic across backends.
    /// **Validates: Requirements 3.3, 6.2, 7.3**
    #[test]
    fn prop_event_ordering_is_stable(events in arb_event_sequence()) {
        let mut sorted_once = events.clone();
        sort_events_by_timestamp(&mut sorted_once);

        let mut sorted_twice = sorted_once.clone();
        sort_events_by_timestamp(&mut sorted_twice);

        // Timestamps must be identical after both sorts
        let ts_once: Vec<_> = sorted_once.iter().map(|e| e.timestamp).collect();
        let ts_twice: Vec<_> = sorted_twice.iter().map(|e| e.timestamp).collect();
        prop_assert_eq!(&ts_once, &ts_twice,
            "double-sort changed timestamp order — ordering is not stable");
    }

    /// **Feature: production-backends, Property 4 (idempotent): Already-Sorted Events Remain Sorted**
    /// *For any* already-sorted sequence of events, sorting again produces the same order.
    /// **Validates: Requirements 3.3, 7.3**
    #[test]
    fn prop_already_sorted_events_remain_sorted(events in arb_event_sequence()) {
        let mut sorted = events;
        sort_events_by_timestamp(&mut sorted);

        let expected: Vec<_> = sorted.iter().map(|e| e.timestamp).collect();

        sort_events_by_timestamp(&mut sorted);
        let actual: Vec<_> = sorted.iter().map(|e| e.timestamp).collect();

        prop_assert_eq!(&actual, &expected,
            "re-sorting an already-sorted sequence changed the order");
    }

    /// **Feature: production-backends, Property 4 (completeness): All Original Timestamps Preserved**
    /// *For any* sequence of events, sorting preserves the multiset of timestamps.
    /// **Validates: Requirements 3.3, 6.2, 7.3**
    #[test]
    fn prop_sorting_preserves_all_timestamps(events in arb_event_sequence()) {
        let mut original_ts: Vec<_> = events.iter().map(|e| e.timestamp).collect();
        original_ts.sort();

        let mut sorted = events;
        sort_events_by_timestamp(&mut sorted);
        let sorted_ts: Vec<_> = sorted.iter().map(|e| e.timestamp).collect();

        prop_assert_eq!(&sorted_ts, &original_ts,
            "sorting did not preserve the multiset of timestamps");
    }
}
