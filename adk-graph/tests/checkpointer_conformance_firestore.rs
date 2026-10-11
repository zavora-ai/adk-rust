//! The Firestore checkpointer passes the shared conformance suite.
//!
//! Run against the Firestore emulator at `FIRESTORE_EMULATOR_HOST`. Each case writes
//! under fresh identifiers, so the cases share one emulator database:
//!
//! ```bash
//! gcloud emulators firestore start --host-port=127.0.0.1:8080 &
//! FIRESTORE_EMULATOR_HOST=127.0.0.1:8080 \
//!   cargo nextest run -p adk-graph --features firestore --run-ignored only \
//!   --test checkpointer_conformance_firestore
//! ```
//!
//! `FIRESTORE_PROJECT_ID` names the emulator project (default `adk-conformance`).

#![cfg(feature = "firestore")]

use std::sync::Arc;

use adk_graph::FirestoreCheckpointer;
use adk_graph::checkpoint::Checkpointer;
use adk_graph::conformance::CheckpointerHarness;

/// A new client per call, as a restarted process would create.
struct Firestore;

#[async_trait::async_trait]
impl CheckpointerHarness for Firestore {
    async fn open(&self) -> Arc<dyn Checkpointer> {
        let project =
            std::env::var("FIRESTORE_PROJECT_ID").unwrap_or_else(|_| "adk-conformance".to_string());
        let checkpointer = FirestoreCheckpointer::connect_emulator(&project)
            .await
            .expect("connect to the Firestore emulator");
        checkpointer.migrate().await.expect("migrate");
        Arc::new(checkpointer)
    }
}

adk_graph::checkpointer_conformance_tests!(
    #[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
    harness = Firestore,
);
