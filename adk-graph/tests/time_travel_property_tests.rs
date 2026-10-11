//! Property tests for time-travel fork independence.
//!
//! **Feature: runtime-reliability-sprint, Property 8: Time-Travel Fork Independence**
//! *For any* fork operation at step S, mutations to the forked thread SHALL NOT
//! affect the original thread's checkpoints.
//! **Validates: Requirements 18.3, 18.4**

#![cfg(feature = "time-travel")]

use std::sync::Arc;

use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::state::Checkpoint;
use proptest::prelude::*;
use serde_json::{Value, json};

// ── Generators ────────────────────────────────────────────────────────

/// Generate a random number of steps (2..=5) for the original thread.
fn arb_step_count() -> impl Strategy<Value = usize> {
    2usize..=5
}

/// Generate a random JSON value for state entries.
fn arb_state_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<i64>().prop_map(Value::from),
        any::<bool>().prop_map(Value::from),
        "[a-zA-Z0-9]{1,10}".prop_map(Value::from),
    ]
}

/// Generate a random number of mutations to apply to the forked thread (1..=5).
fn arb_mutation_count() -> impl Strategy<Value = usize> {
    1usize..=5
}

// ── Property 8: Fork Independence ────────────────────────────────────
//
// *For any* fork operation at step S, mutations to the forked thread
// SHALL NOT affect the original thread's checkpoints.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: runtime-reliability-sprint, Property 8: Time-Travel Fork Independence**
    /// *For any* fork at step S, saving new checkpoints to the forked thread
    /// SHALL NOT change the original thread's checkpoint count or state values.
    /// **Validates: Requirements 18.3, 18.4**
    #[test]
    fn prop_fork_does_not_mutate_original_thread(
        step_count in arb_step_count(),
        state_value in arb_state_value(),
        mutation_count in arb_mutation_count(),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let checkpointer = Arc::new(MemoryCheckpointer::new());

            // 1. Save several checkpoints for "original_thread"
            for step in 0..step_count {
                let mut state = std::collections::HashMap::new();
                state.insert("counter".to_string(), json!(step));
                state.insert("data".to_string(), state_value.clone());
                let cp = Checkpoint::new(
                    "original_thread",
                    state,
                    step,
                    vec!["node_a".to_string()],
                )
                .with_version(step as u64 + 1);
                checkpointer.save(&cp).await.unwrap();
            }

            // 2. Snapshot the original thread's checkpoints before fork
            let original_before = checkpointer.list("original_thread").await.unwrap();
            let original_count_before = original_before.len();
            let original_states_before: Vec<_> = original_before
                .iter()
                .map(|cp| (cp.step, cp.state.clone()))
                .collect();

            // 3. Determine fork point (must be within valid range)
            let fork_point = step_count.min(original_count_before) - 1;
            // Use fork_at logic: find checkpoint at fork_point step and save under new thread
            let fork_checkpoint = original_before
                .iter()
                .find(|cp| cp.step == fork_point)
                .unwrap();

            let forked_cp = Checkpoint::new(
                "forked_thread",
                fork_checkpoint.state.clone(),
                fork_checkpoint.step,
                fork_checkpoint.pending_nodes.clone(),
            );
            checkpointer.save(&forked_cp).await.unwrap();

            // 4. Mutate the forked thread by saving additional checkpoints
            for i in 0..mutation_count {
                let mut mutated_state = std::collections::HashMap::new();
                mutated_state.insert("counter".to_string(), json!(1000 + i));
                mutated_state.insert("data".to_string(), json!("mutated"));
                mutated_state.insert("extra_key".to_string(), json!(format!("extra_{i}")));
                let mutated_cp = Checkpoint::new(
                    "forked_thread",
                    mutated_state,
                    fork_point + 1 + i,
                    vec!["mutated_node".to_string()],
                )
                .with_version(i as u64 + 2);
                checkpointer.save(&mutated_cp).await.unwrap();
            }

            // 5. Verify original thread is completely unchanged
            let original_after = checkpointer.list("original_thread").await.unwrap();

            // Same count
            prop_assert_eq!(
                original_after.len(),
                original_count_before,
                "Original thread checkpoint count changed after fork mutations"
            );

            // Same state at each step
            let original_states_after: Vec<_> = original_after
                .iter()
                .map(|cp| (cp.step, cp.state.clone()))
                .collect();

            prop_assert_eq!(
                &original_states_after,
                &original_states_before,
                "Original thread state changed after fork mutations"
            );

            // Additionally verify the forked thread has the expected count
            let forked_checkpoints = checkpointer.list("forked_thread").await.unwrap();
            prop_assert_eq!(
                forked_checkpoints.len(),
                1 + mutation_count,
                "Forked thread should have initial fork + mutation checkpoints"
            );

            Ok(())
        })?;
    }

    /// **Feature: runtime-reliability-sprint, Property 8: Time-Travel Fork Independence**
    /// *For any* fork at a random step S within a thread of N steps, the original
    /// thread's checkpoint states at every step SHALL remain byte-identical after
    /// arbitrary mutations to the forked thread.
    /// **Validates: Requirements 18.3, 18.4**
    #[test]
    fn prop_fork_at_random_step_preserves_all_original_checkpoints(
        step_count in arb_step_count(),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let checkpointer = Arc::new(MemoryCheckpointer::new());

            // 1. Create checkpoints with distinct state at each step
            for step in 0..step_count {
                let mut state = std::collections::HashMap::new();
                state.insert("step_value".to_string(), json!(step * 10));
                state.insert("label".to_string(), json!(format!("step_{step}")));
                let cp = Checkpoint::new(
                    "original_thread",
                    state,
                    step,
                    vec![format!("node_{step}")],
                )
                .with_version(step as u64 + 1);
                checkpointer.save(&cp).await.unwrap();
            }

            // 2. Pick a random fork point (use step_count / 2 as deterministic mid-point)
            let fork_point = step_count / 2;

            // 3. Snapshot original state
            let original_before = checkpointer.list("original_thread").await.unwrap();
            let serialized_before = serde_json::to_string(&original_before
                .iter()
                .map(|cp| (&cp.step, &cp.state, &cp.pending_nodes))
                .collect::<Vec<_>>()
            ).unwrap();

            // 4. Fork at the chosen step
            let fork_cp = original_before
                .iter()
                .find(|cp| cp.step == fork_point)
                .unwrap();
            let forked = Checkpoint::new(
                "forked_thread",
                fork_cp.state.clone(),
                fork_cp.step,
                fork_cp.pending_nodes.clone(),
            );
            checkpointer.save(&forked).await.unwrap();

            // 5. Heavily mutate the forked thread
            for i in 0..5 {
                let mut state = std::collections::HashMap::new();
                state.insert("step_value".to_string(), json!(9999));
                state.insert("label".to_string(), json!("FORKED_MUTATION"));
                state.insert("new_field".to_string(), json!(i));
                let cp = Checkpoint::new(
                    "forked_thread",
                    state,
                    fork_point + 1 + i,
                    vec!["forked_node".to_string()],
                )
                .with_version(i as u64 + 2);
                checkpointer.save(&cp).await.unwrap();
            }

            // 6. Verify original is byte-identical
            let original_after = checkpointer.list("original_thread").await.unwrap();
            let serialized_after = serde_json::to_string(&original_after
                .iter()
                .map(|cp| (&cp.step, &cp.state, &cp.pending_nodes))
                .collect::<Vec<_>>()
            ).unwrap();

            prop_assert_eq!(
                &serialized_after,
                &serialized_before,
                "Original thread checkpoints were modified by forked thread mutations"
            );

            Ok(())
        })?;
    }
}
