//! Direct span export to a local SQLite database — zero-infrastructure tracing.
//!
//! [`SqliteSpanExporter`] persists agent spans to a single `.db` file with no
//! collector or backend to deploy, and [`SqliteTraceReader`] queries them back
//! for inspection or visualization.
//!
//! Writes never block the traced code path: spans are handed to a dedicated
//! writer thread over an unbounded channel and committed in batched
//! transactions (WAL mode), so the cost inside `tracing`'s `on_close` is one
//! channel send.
//!
//! ```no_run
//! use adk_telemetry::init_with_sqlite;
//!
//! let exporter = init_with_sqlite("my-agent", "traces.db")
//!     .expect("failed to initialize telemetry");
//! // ... run your agent ...
//! exporter.flush().ok(); // make sure everything hit the database
//! ```
//!
//! Inspect the results from code (or any SQLite client — the schema is a
//! single `spans` table with an `attributes` JSON column):
//!
//! ```no_run
//! use adk_telemetry::sqlite::SqliteTraceReader;
//!
//! let reader = SqliteTraceReader::open("traces.db").unwrap();
//! for session in reader.sessions().unwrap() {
//!     println!("{}: {} spans", session.session_id, session.span_count);
//! }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, params};

use crate::init::TelemetryError;
use crate::span_exporter::SpanSink;

/// Maximum spans per write transaction.
const BATCH_SIZE: usize = 64;
/// How long the writer waits for more spans before committing a partial batch.
const BATCH_INTERVAL: Duration = Duration::from_millis(250);
/// How long [`SqliteSpanExporter::flush`] waits for the writer to acknowledge.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

/// Attribute keys promoted to indexed columns (also kept in the JSON blob).
const SESSION_ID_KEY: &str = "gcp.vertex.agent.session_id";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS spans (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    trace_id    TEXT NOT NULL,
    span_id     TEXT NOT NULL,
    session_id  TEXT,
    span_name   TEXT NOT NULL,
    start_time_unix_nanos INTEGER NOT NULL,
    end_time_unix_nanos   INTEGER NOT NULL,
    attributes  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_spans_trace   ON spans(trace_id);
CREATE INDEX IF NOT EXISTS idx_spans_session ON spans(session_id);
CREATE INDEX IF NOT EXISTS idx_spans_name    ON spans(span_name);
";

enum WriterMsg {
    Span { name: String, attributes: HashMap<String, String> },
    Flush(Sender<()>),
    Shutdown,
}

/// [`SpanSink`] that persists spans to a local SQLite file.
///
/// Create with [`SqliteSpanExporter::new`] (or the
/// [`init_with_sqlite`](crate::init_with_sqlite) convenience helper) and hand
/// it to [`AdkSpanLayer`](crate::span_exporter::AdkSpanLayer).
///
/// By default only the agent-loop spans are stored (`agent.execute`,
/// `call_llm`, `send_data`, `execute_tool*`) — the same set the in-memory
/// [`AdkSpanExporter`](crate::span_exporter::AdkSpanExporter) keeps. Call
/// [`record_all_spans`](Self::record_all_spans) to persist every span the
/// active `EnvFilter` lets through.
pub struct SqliteSpanExporter {
    tx: Sender<WriterMsg>,
    writer: Mutex<Option<JoinHandle<()>>>,
    record_all: bool,
    path: PathBuf,
}

impl SqliteSpanExporter {
    /// Open (or create) the database at `path` and start the writer thread.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, TelemetryError> {
        let path = path.as_ref().to_path_buf();
        let conn = open_writer_connection(&path)?;

        let (tx, rx) = channel::<WriterMsg>();
        let writer = std::thread::Builder::new()
            .name("adk-telemetry-sqlite".into())
            .spawn(move || writer_loop(conn, rx))
            .map_err(|e| TelemetryError::Init(format!("failed to spawn sqlite writer: {e}")))?;

        Ok(Self { tx, writer: Mutex::new(Some(writer)), record_all: false, path })
    }

    /// Persist every span enabled by the subscriber's filter instead of only
    /// the agent-loop allowlist.
    pub fn record_all_spans(mut self, record_all: bool) -> Self {
        self.record_all = record_all;
        self
    }

    /// Path of the database file this exporter writes to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Block until every span handed to the exporter so far is committed.
    pub fn flush(&self) -> Result<(), TelemetryError> {
        let (ack_tx, ack_rx) = channel();
        self.tx
            .send(WriterMsg::Flush(ack_tx))
            .map_err(|_| TelemetryError::Init("sqlite writer thread is gone".into()))?;
        ack_rx
            .recv_timeout(FLUSH_TIMEOUT)
            .map_err(|_| TelemetryError::Init("sqlite flush timed out".into()))
    }

    /// Flush pending spans and stop the writer thread.
    ///
    /// Called automatically on drop; explicit shutdown is useful when the
    /// exporter is held alive by a global subscriber.
    pub fn shutdown(&self) {
        let _ = self.tx.send(WriterMsg::Shutdown);
        if let Some(handle) = self.writer.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }
    }
}

impl Drop for SqliteSpanExporter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl std::fmt::Debug for SqliteSpanExporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSpanExporter")
            .field("path", &self.path)
            .field("record_all", &self.record_all)
            .finish()
    }
}

impl SpanSink for SqliteSpanExporter {
    fn export_span(&self, span_name: &str, attributes: HashMap<String, String>) {
        if !self.record_all && !is_agent_loop_span(span_name) {
            return;
        }
        // Unbounded channel: never blocks the traced thread. If the writer
        // died, dropping the span is the only safe option.
        let _ = self.tx.send(WriterMsg::Span { name: span_name.to_string(), attributes });
    }
}

fn is_agent_loop_span(span_name: &str) -> bool {
    span_name == "agent.execute"
        || span_name == "call_llm"
        || span_name == "send_data"
        || span_name.starts_with("execute_tool")
}

fn open_writer_connection(path: &Path) -> Result<Connection, TelemetryError> {
    let conn = Connection::open(path)
        .map_err(|e| TelemetryError::Init(format!("failed to open sqlite db: {e}")))?;
    // WAL keeps readers (the visualizer) from blocking the writer and
    // vice versa; NORMAL sync is durable enough for trace data.
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| TelemetryError::Init(format!("failed to enable WAL: {e}")))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| TelemetryError::Init(format!("failed to set synchronous: {e}")))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| TelemetryError::Init(format!("failed to set busy timeout: {e}")))?;
    conn.execute_batch(SCHEMA)
        .map_err(|e| TelemetryError::Init(format!("failed to create schema: {e}")))?;
    Ok(conn)
}

fn writer_loop(mut conn: Connection, rx: Receiver<WriterMsg>) {
    let mut batch: Vec<(String, HashMap<String, String>)> = Vec::with_capacity(BATCH_SIZE);

    loop {
        match rx.recv_timeout(BATCH_INTERVAL) {
            Ok(WriterMsg::Span { name, attributes }) => {
                batch.push((name, attributes));
                if batch.len() >= BATCH_SIZE {
                    write_batch(&mut conn, &mut batch);
                }
            }
            Ok(WriterMsg::Flush(ack)) => {
                write_batch(&mut conn, &mut batch);
                let _ = ack.send(());
            }
            Ok(WriterMsg::Shutdown) => {
                write_batch(&mut conn, &mut batch);
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                write_batch(&mut conn, &mut batch);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                write_batch(&mut conn, &mut batch);
                break;
            }
        }
    }
}

fn write_batch(conn: &mut Connection, batch: &mut Vec<(String, HashMap<String, String>)>) {
    if batch.is_empty() {
        return;
    }
    let result = (|| -> rusqlite::Result<()> {
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO spans (trace_id, span_id, session_id, span_name,
                                    start_time_unix_nanos, end_time_unix_nanos, attributes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (name, attributes) in batch.iter() {
                let trace_id = attributes.get("trace_id").cloned().unwrap_or_default();
                let span_id = attributes.get("span_id").cloned().unwrap_or_default();
                let session_id = attributes.get(SESSION_ID_KEY).cloned();
                let start = attr_nanos(attributes, "start_time");
                let end = attr_nanos(attributes, "end_time");
                let json = serde_json::to_string(attributes).unwrap_or_else(|_| "{}".into());
                stmt.execute(params![trace_id, span_id, session_id, name, start, end, json])?;
            }
        }
        tx.commit()
    })();

    if let Err(e) = result {
        // Events (not spans) can't recurse into the span layer, so logging
        // from the writer thread is safe.
        tracing::warn!("adk-telemetry sqlite writer failed to commit batch: {e}");
    }
    batch.clear();
}

fn attr_nanos(attributes: &HashMap<String, String>, key: &str) -> i64 {
    attributes.get(key).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Reading back
// ---------------------------------------------------------------------------

/// One span row read back from the database.
#[derive(Debug, Clone)]
pub struct SpanRow {
    /// Trace (invocation) this span belongs to.
    pub trace_id: String,
    /// Unique span id (the agent event id when available).
    pub span_id: String,
    /// Session the span belongs to, if recorded.
    pub session_id: Option<String>,
    /// Span name (e.g. `call_llm`).
    pub span_name: String,
    /// Wall-clock start, nanoseconds since the Unix epoch.
    pub start_time_unix_nanos: i64,
    /// Wall-clock end, nanoseconds since the Unix epoch.
    pub end_time_unix_nanos: i64,
    /// All captured attributes.
    pub attributes: HashMap<String, String>,
}

impl SpanRow {
    /// Span duration in nanoseconds.
    pub fn duration_nanos(&self) -> i64 {
        self.end_time_unix_nanos - self.start_time_unix_nanos
    }
}

/// Per-session aggregate, for listing what's in a trace database.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    /// Session id.
    pub session_id: String,
    /// Number of spans recorded for the session.
    pub span_count: u64,
    /// Earliest span start in the session (Unix nanos).
    pub first_start_unix_nanos: i64,
    /// Latest span end in the session (Unix nanos).
    pub last_end_unix_nanos: i64,
}

/// Read-only access to a span database written by [`SqliteSpanExporter`].
pub struct SqliteTraceReader {
    conn: Connection,
}

impl SqliteTraceReader {
    /// Open the database at `path` read-only.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TelemetryError> {
        let conn = Connection::open_with_flags(
            path.as_ref(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| TelemetryError::Init(format!("failed to open sqlite db read-only: {e}")))?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| TelemetryError::Init(format!("failed to set busy timeout: {e}")))?;
        Ok(Self { conn })
    }

    /// Sessions present in the database, most recent first.
    pub fn sessions(&self) -> Result<Vec<SessionSummary>, TelemetryError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session_id, COUNT(*), MIN(start_time_unix_nanos), MAX(end_time_unix_nanos)
                 FROM spans WHERE session_id IS NOT NULL
                 GROUP BY session_id ORDER BY MAX(end_time_unix_nanos) DESC",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SessionSummary {
                    session_id: row.get(0)?,
                    span_count: row.get(1)?,
                    first_start_unix_nanos: row.get(2)?,
                    last_end_unix_nanos: row.get(3)?,
                })
            })
            .map_err(sql_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
    }

    /// All spans for a session, oldest first.
    pub fn session_trace(&self, session_id: &str) -> Result<Vec<SpanRow>, TelemetryError> {
        self.query_spans(
            "SELECT trace_id, span_id, session_id, span_name,
                    start_time_unix_nanos, end_time_unix_nanos, attributes
             FROM spans WHERE session_id = ?1 ORDER BY start_time_unix_nanos",
            params![session_id],
        )
    }

    /// All spans for a trace (invocation), oldest first.
    pub fn trace(&self, trace_id: &str) -> Result<Vec<SpanRow>, TelemetryError> {
        self.query_spans(
            "SELECT trace_id, span_id, session_id, span_name,
                    start_time_unix_nanos, end_time_unix_nanos, attributes
             FROM spans WHERE trace_id = ?1 ORDER BY start_time_unix_nanos",
            params![trace_id],
        )
    }

    /// The most recent `limit` spans, newest first.
    pub fn recent_spans(&self, limit: u32) -> Result<Vec<SpanRow>, TelemetryError> {
        self.query_spans(
            "SELECT trace_id, span_id, session_id, span_name,
                    start_time_unix_nanos, end_time_unix_nanos, attributes
             FROM spans ORDER BY end_time_unix_nanos DESC LIMIT ?1",
            params![limit],
        )
    }

    fn query_spans(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<SpanRow>, TelemetryError> {
        let mut stmt = self.conn.prepare(sql).map_err(sql_err)?;
        let rows = stmt
            .query_map(params, |row| {
                let json: String = row.get(6)?;
                Ok(SpanRow {
                    trace_id: row.get(0)?,
                    span_id: row.get(1)?,
                    session_id: row.get(2)?,
                    span_name: row.get(3)?,
                    start_time_unix_nanos: row.get(4)?,
                    end_time_unix_nanos: row.get(5)?,
                    attributes: serde_json::from_str(&json).unwrap_or_default(),
                })
            })
            .map_err(sql_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
    }
}

fn sql_err(e: rusqlite::Error) -> TelemetryError {
    TelemetryError::Init(format!("sqlite query failed: {e}"))
}
