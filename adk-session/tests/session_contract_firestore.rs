//! The shared session contract on the Firestore backend.
//!
//! Run against the Firestore emulator at `FIRESTORE_EMULATOR_HOST`; each run uses fresh app
//! names:
//!
//! ```bash
//! gcloud emulators firestore start --host-port=127.0.0.1:8080 &
//! FIRESTORE_EMULATOR_HOST=127.0.0.1:8080 \
//!   cargo nextest run -p adk-session --features firestore --run-ignored only \
//!   --test session_contract_firestore
//! ```
//!
//! `FIRESTORE_PROJECT_ID` names the emulator project (default `adk-conformance`).

#![cfg(feature = "firestore")]

mod common;

use adk_session::FirestoreSessionService;
use adk_session::firestore_stores::connect_firestore_emulator;

async fn service() -> FirestoreSessionService {
    let project =
        std::env::var("FIRESTORE_PROJECT_ID").unwrap_or_else(|_| "adk-conformance".to_string());
    let db = connect_firestore_emulator(&project).await.expect("connect to the Firestore emulator");
    FirestoreSessionService::from_db(db, None)
}

fn run_app(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test]
#[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
async fn firestore_session_contract() {
    common::session_contract::assert_session_contract(
        &service().await,
        &run_app("contract"),
        &run_app("contract-other"),
    )
    .await;
}

#[tokio::test]
#[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
async fn firestore_session_integrity_contract() {
    let service = service().await;
    let app = run_app("integrity");
    common::integrity_contract::assert_create_and_append_errors_contract(&service, &app).await;
    common::integrity_contract::assert_event_order_contract(&service, &app).await;
    common::integrity_contract::assert_nul_round_trip_contract(&service, &app).await;
    common::integrity_contract::assert_list_order_contract(&service, &app).await;
    common::integrity_contract::assert_rewind_unsupported(&service, &app).await;
}

#[tokio::test]
#[ignore = "requires the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
async fn firestore_delete_all_sessions_erases_the_user_tier() {
    common::state_contract::assert_delete_all_sessions_contract(
        &service().await,
        &run_app("erase"),
        &run_app("erase-other"),
        true,
    )
    .await;
}
