//! End-to-end tests for the SQLite span exporter: spans emitted through a real
//! tracing subscriber must land in the database and read back correctly.
#![cfg(feature = "sqlite")]

use std::sync::Arc;

use adk_telemetry::span_exporter::AdkSpanLayer;
use adk_telemetry::sqlite::{SqliteSpanExporter, SqliteTraceReader};
use tracing_subscriber::layer::SubscriberExt;

fn temp_db(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("adk-telemetry-sqlite-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn emit_agent_session(session_id: &str, invocation_id: &str) {
    let parent = tracing::info_span!(
        "agent.execute",
        "gcp.vertex.agent.event_id" = format!("evt-{invocation_id}-agent"),
        "gcp.vertex.agent.invocation_id" = invocation_id,
        "gcp.vertex.agent.session_id" = session_id,
        "agent.name" = "test-agent"
    );
    let _parent_guard = parent.enter();

    let llm = tracing::info_span!(
        "call_llm",
        "gcp.vertex.agent.event_id" = format!("evt-{invocation_id}-llm"),
        "gcp.vertex.agent.llm_request" = "{}"
    );
    drop(llm.entered());

    // Not on the agent-loop allowlist — must NOT be stored by default.
    let noise = tracing::info_span!("internal.bookkeeping");
    drop(noise.entered());
}

#[test]
fn spans_round_trip_through_sqlite() {
    let db = temp_db("round-trip");
    let exporter = Arc::new(SqliteSpanExporter::new(&db).unwrap());
    let layer = AdkSpanLayer::new(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        emit_agent_session("session-1", "inv-1");
        emit_agent_session("session-2", "inv-2");
    });

    exporter.flush().unwrap();

    let reader = SqliteTraceReader::open(&db).unwrap();

    // Two sessions, two allowlisted spans each; the noise span is filtered.
    let sessions = reader.sessions().unwrap();
    assert_eq!(sessions.len(), 2, "expected two sessions, got {sessions:?}");
    for s in &sessions {
        assert_eq!(s.span_count, 2, "session {s:?}");
    }

    let trace = reader.session_trace("session-1").unwrap();
    assert_eq!(trace.len(), 2);
    let names: Vec<_> = trace.iter().map(|s| s.span_name.as_str()).collect();
    assert!(names.contains(&"agent.execute"), "{names:?}");
    assert!(names.contains(&"call_llm"), "{names:?}");

    // Child span inherited the session id from its parent.
    let llm = trace.iter().find(|s| s.span_name == "call_llm").unwrap();
    assert_eq!(llm.session_id.as_deref(), Some("session-1"));
    assert_eq!(llm.trace_id, "inv-1");
    assert_eq!(llm.span_id, "evt-inv-1-llm");
    assert!(llm.end_time_unix_nanos >= llm.start_time_unix_nanos);
    assert_eq!(llm.attributes.get("gcp.vertex.agent.llm_request").map(String::as_str), Some("{}"));

    // Trace lookup by invocation id.
    assert_eq!(reader.trace("inv-2").unwrap().len(), 2);

    // recent_spans returns newest first and respects the limit.
    let recent = reader.recent_spans(3).unwrap();
    assert_eq!(recent.len(), 3);

    let _ = std::fs::remove_file(&db);
}

#[test]
fn record_all_spans_keeps_non_agent_spans() {
    let db = temp_db("record-all");
    let exporter = Arc::new(SqliteSpanExporter::new(&db).unwrap().record_all_spans(true));
    let layer = AdkSpanLayer::new(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("custom.work", step = "one");
        drop(span.entered());
    });

    exporter.flush().unwrap();

    let reader = SqliteTraceReader::open(&db).unwrap();
    let recent = reader.recent_spans(10).unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].span_name, "custom.work");
    assert_eq!(recent[0].attributes.get("step").map(String::as_str), Some("one"));

    let _ = std::fs::remove_file(&db);
}

#[test]
fn flush_is_idempotent_and_shutdown_safe() {
    let db = temp_db("shutdown");
    let exporter = Arc::new(SqliteSpanExporter::new(&db).unwrap());
    let layer = AdkSpanLayer::new(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        emit_agent_session("session-x", "inv-x");
    });

    exporter.flush().unwrap();
    exporter.flush().unwrap();
    exporter.shutdown();
    // After shutdown the data is still readable and complete.
    let reader = SqliteTraceReader::open(&db).unwrap();
    assert_eq!(reader.session_trace("session-x").unwrap().len(), 2);

    let _ = std::fs::remove_file(&db);
}
