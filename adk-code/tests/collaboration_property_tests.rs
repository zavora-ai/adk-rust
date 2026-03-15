//! Property-based tests for correlated collaboration flow.
//!
//! **Feature: code-execution, Property 1: Collaboration Correlation Is Preserved**
//! *For any* valid correlation_id, topic, and producer strings, when an agent publishes
//! a NeedWork event and another agent publishes a WorkPublished event with the same
//! correlation_id, wait_for_work returns the matching event with the correct correlation_id.
//! **Validates: Requirements 12.5, 13.4, 13.5, 13.8**
//!
//! **Feature: code-execution, Property 2: Blocked Agents Resume Only On Matching Work**
//! *For any* set of correlation IDs, when multiple agents publish events with different
//! correlation IDs, wait_for_kind only resumes on the exact matching correlation_id AND
//! kind combination. Events with wrong correlation or wrong kind do not cause premature resume.
//! **Validates: Requirements 12.5, 13.4, 13.5, 13.8**

use adk_code::{CollaborationEventKind, Workspace};
use proptest::prelude::*;
use std::time::Duration;

/// Generate non-empty alphanumeric strings suitable for correlation IDs, topics, and producers.
fn arb_identifier() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_-]{1,30}"
}

/// Generate an arbitrary collaboration event kind.
fn arb_event_kind() -> impl Strategy<Value = CollaborationEventKind> {
    prop_oneof![
        Just(CollaborationEventKind::NeedWork),
        Just(CollaborationEventKind::WorkClaimed),
        Just(CollaborationEventKind::WorkPublished),
        Just(CollaborationEventKind::FeedbackRequested),
        Just(CollaborationEventKind::FeedbackProvided),
        Just(CollaborationEventKind::Blocked),
        Just(CollaborationEventKind::Completed),
    ]
}

// ── Property 1: Collaboration Correlation Is Preserved ─────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: code-execution, Property 1: Collaboration Correlation Is Preserved**
    /// *For any* valid correlation_id, topic, and producer strings, when an agent
    /// publishes a NeedWork event and another agent publishes a WorkPublished event
    /// with the same correlation_id, wait_for_work returns the matching event with
    /// the correct correlation_id.
    /// **Validates: Requirements 12.5, 13.4, 13.5, 13.8**
    #[test]
    fn prop_collaboration_correlation_is_preserved(
        correlation_id in arb_identifier(),
        topic in arb_identifier(),
        requester in arb_identifier(),
        publisher in arb_identifier(),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async {
            let ws = Workspace::new("/tmp/prop-test").build();
            let ws_pub = ws.clone();

            let corr = correlation_id.clone();
            let t = topic.clone();
            let p = publisher.clone();

            // Agent A requests work.
            ws.request_work(&correlation_id, &topic, &requester);

            // Agent B publishes matching work after a short delay.
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                ws_pub.publish_work(&corr, &t, &p, serde_json::json!({ "result": "ok" }));
            });

            // Agent A waits for the correlated work.
            let result = ws.wait_for_work(&correlation_id, Duration::from_secs(2)).await;
            let event = result.expect("should receive matching WorkPublished event");

            prop_assert_eq!(&event.correlation_id, &correlation_id);
            prop_assert_eq!(event.kind, CollaborationEventKind::WorkPublished);
            prop_assert_eq!(&event.topic, &topic);
            prop_assert_eq!(&event.producer, &publisher);
            prop_assert_eq!(event.payload, serde_json::json!({ "result": "ok" }));

            Ok(())
        })?;
    }
}

// ── Property 2: Blocked Agents Resume Only On Matching Work ────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: code-execution, Property 2: Blocked Agents Resume Only On Matching Work**
    /// *For any* set of correlation IDs, when multiple agents publish events with
    /// different correlation IDs, wait_for_kind only resumes on the exact matching
    /// correlation_id AND kind combination. Events with wrong correlation or wrong
    /// kind do not cause premature resume.
    /// **Validates: Requirements 12.5, 13.4, 13.5, 13.8**
    #[test]
    fn prop_blocked_agents_resume_only_on_matching_work(
        target_corr in arb_identifier(),
        wrong_corr in arb_identifier(),
        target_kind in arb_event_kind(),
        wrong_kind in arb_event_kind(),
        topic in arb_identifier(),
        producer in arb_identifier(),
    ) {
        // Ensure the wrong correlation and wrong kind are actually different
        // from the target so the test is meaningful.
        prop_assume!(target_corr != wrong_corr);
        prop_assume!(target_kind != wrong_kind);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async {
            let ws = Workspace::new("/tmp/prop-test-2").build();
            let ws_pub = ws.clone();

            let tc = target_corr.clone();
            let wc = wrong_corr.clone();
            let tk = target_kind;
            let wk = wrong_kind;
            let t = topic.clone();
            let p = producer.clone();

            // Spawn a publisher that sends non-matching events first, then the match.
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(5)).await;

                // Wrong correlation, right kind — should NOT resume the waiter.
                ws_pub.publish(adk_code::CollaborationEvent::new(
                    &wc, &t, &p, tk,
                ));

                // Right correlation, wrong kind — should NOT resume the waiter.
                ws_pub.publish(adk_code::CollaborationEvent::new(
                    &tc, &t, &p, wk,
                ));

                // Both wrong — should NOT resume the waiter.
                ws_pub.publish(adk_code::CollaborationEvent::new(
                    &wc, &t, &p, wk,
                ));

                tokio::time::sleep(Duration::from_millis(5)).await;

                // Exact match — should resume the waiter.
                ws_pub.publish(adk_code::CollaborationEvent::new(
                    &tc, &t, &p, tk,
                ));
            });

            let result = ws
                .wait_for_kind(&target_corr, target_kind, Duration::from_secs(2))
                .await;
            let event = result.expect("should resume only on exact match");

            prop_assert_eq!(&event.correlation_id, &target_corr);
            prop_assert_eq!(event.kind, target_kind);
            prop_assert_eq!(&event.topic, &topic);
            prop_assert_eq!(&event.producer, &producer);

            Ok(())
        })?;
    }
}
