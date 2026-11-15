//! Keyless long-history benchmark.
//!
//! A scripted model with no latency drives an `LlmAgent` through `Runner` on an in-memory
//! session, so the framework's own cost per turn or per tool iteration is measured as the
//! history grows, with no API key and no provider time in the numbers. Framework work that
//! copies or rescans the whole history shows as per-step cost that rises with the step number.
//!
//! | Mode | One step |
//! |------|----------|
//! | [`LongHistoryMode::Turns`] | One `Runner::run`: a user message, one tool call, and the answer |
//! | [`LongHistoryMode::Loop`] | One model call and tool call inside a single `Runner::run` whose model calls the tool `steps` times |
//!
//! # Example
//!
//! ```rust
//! use adk_bench::long_history::{LongHistoryConfig, run_long_history};
//!
//! # #[tokio::main]
//! # async fn main() -> adk_bench::Result<()> {
//! let config = LongHistoryConfig { steps: 20, ..LongHistoryConfig::default() };
//! let report = run_long_history(&config).await?;
//! assert_eq!(report.step_wall.len(), 20);
//! assert_eq!(report.session_events, 80);
//! # Ok(())
//! # }
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, RunConfig,
    SessionId, Tool, ToolContext, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};

use crate::{BenchError, DurationStats, Result, compute_stats};

const APP: &str = "long-history";
const USER: &str = "bench";
const SESSION: &str = "long-history";

/// What one step of a [`run_long_history`] run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LongHistoryMode {
    /// Each step is one `Runner::run` on the same session: a user message, one tool call and
    /// the answer, adding four events.
    Turns,
    /// One `Runner::run` whose model calls the tool once per step, then answers: an autonomous
    /// run whose conversation grows inside one invocation.
    Loop,
}

/// Configuration of a [`run_long_history`] run.
#[derive(Debug, Clone)]
pub struct LongHistoryConfig {
    /// What a step is.
    pub mode: LongHistoryMode,
    /// Number of steps: turns, or tool iterations of the one run.
    pub steps: usize,
    /// Size of the text each tool result carries.
    pub tool_payload_bytes: usize,
    /// `RunConfig::history_max_events` for every run; `None` loads the whole session.
    pub history_max_events: Option<usize>,
}

impl Default for LongHistoryConfig {
    fn default() -> Self {
        Self {
            mode: LongHistoryMode::Turns,
            steps: 1_000,
            tool_payload_bytes: 200,
            history_max_events: None,
        }
    }
}

/// Measurements of a [`run_long_history`] run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongHistoryReport {
    /// What a step was.
    pub mode: LongHistoryMode,
    /// Events stored in the session when the run ended.
    pub session_events: usize,
    /// Wall time of each step, in order.
    #[serde(skip)]
    pub step_wall: Vec<Duration>,
    /// Process CPU time of each step, in order; empty where the platform has no per-process
    /// CPU clock.
    #[serde(skip)]
    pub step_cpu: Vec<Duration>,
    /// Statistics of the first tenth of the steps (at least one): the cost on a short history.
    pub first: DurationStats,
    /// Statistics of the last tenth of the steps: the cost on the full history.
    pub last: DurationStats,
    /// Process CPU time of the whole run, where the platform reports it.
    pub total_cpu: Option<Duration>,
    /// Peak resident set size of the process, where the platform reports it.
    pub peak_rss_bytes: Option<u64>,
}

impl LongHistoryReport {
    /// How many times the mean cost of the last tenth of the steps is the first tenth's.
    ///
    /// Process CPU time is compared where the platform reports it, else wall time. A value
    /// near 1 means the per-step cost does not grow with the history.
    pub fn growth(&self) -> f64 {
        self.last.mean_us.max(1) as f64 / self.first.mean_us.max(1) as f64
    }
}

/// A scripted model with no latency: in [`LongHistoryMode::Turns`] it calls the tool on every
/// other call and answers on the rest; in [`LongHistoryMode::Loop`] it calls the tool `steps`
/// times, then answers. It records the moment of each call and keeps nothing else.
struct ScriptedModel {
    mode: LongHistoryMode,
    steps: usize,
    calls: AtomicUsize,
    stamps: Mutex<Vec<(Instant, Option<Duration>)>>,
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(
        &self,
        _request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.stamps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((Instant::now(), process_cpu_time()));
        let calls_tool = match self.mode {
            LongHistoryMode::Turns => call.is_multiple_of(2),
            LongHistoryMode::Loop => call < self.steps,
        };
        let part = if calls_tool {
            Part::FunctionCall {
                name: "read_chunk".to_string(),
                args: json!({ "chunk": call }),
                id: Some(format!("call_{call}")),
                thought_signature: None,
            }
        } else {
            Part::Text { text: "Done.".to_string() }
        };
        let mut response =
            LlmResponse::new(Content { role: "model".to_string(), parts: vec![part] });
        response.finish_reason = Some(FinishReason::Stop);
        response.turn_complete = true;
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// A tool that returns a fixed payload.
struct ReadChunk {
    payload: String,
}

#[async_trait]
impl Tool for ReadChunk {
    fn name(&self) -> &str {
        "read_chunk"
    }

    fn description(&self) -> &str {
        "Reads the next chunk of a file."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({ "type": "object", "properties": { "chunk": { "type": "integer" } } }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        Ok(json!({ "chunk": args["chunk"], "data": self.payload }))
    }
}

/// The agent, runner and session of a long-history run, driven one step at a time.
///
/// [`run_long_history`] times the steps; drive the bench directly to measure something else
/// per step, such as allocations.
///
/// # Example
///
/// ```rust
/// use adk_bench::long_history::{LongHistoryBench, LongHistoryConfig};
///
/// # #[tokio::main]
/// # async fn main() -> adk_bench::Result<()> {
/// let bench = LongHistoryBench::new(&LongHistoryConfig::default()).await?;
/// for _ in 0..3 {
///     bench.turn().await?;
/// }
/// assert_eq!(bench.session_events().await?, 12);
/// # Ok(())
/// # }
/// ```
pub struct LongHistoryBench {
    model: Arc<ScriptedModel>,
    runner: Runner,
    sessions: Arc<InMemorySessionService>,
    turns: AtomicUsize,
}

impl LongHistoryBench {
    /// Builds the agent, runner and empty session for `config`.
    ///
    /// # Errors
    ///
    /// Returns [`BenchError::Llm`] when the agent, the runner or the session cannot be built.
    pub async fn new(config: &LongHistoryConfig) -> Result<Self> {
        let model = Arc::new(ScriptedModel {
            mode: config.mode,
            steps: config.steps,
            calls: AtomicUsize::new(0),
            stamps: Mutex::new(Vec::with_capacity(2 * config.steps + 2)),
        });
        let agent = LlmAgentBuilder::new("worker")
            .model(Arc::clone(&model) as Arc<dyn Llm>)
            .instruction("Read the file one chunk at a time with read_chunk.")
            .tool(Arc::new(ReadChunk { payload: "x".repeat(config.tool_payload_bytes) }))
            .max_iterations(u32::try_from(config.steps).unwrap_or(u32::MAX).saturating_add(10))
            .build()
            .map_err(|error| BenchError::Llm(error.to_string()))?;
        let sessions = Arc::new(InMemorySessionService::new());
        sessions
            .create(CreateRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: Some(SESSION.to_string()),
                state: Default::default(),
            })
            .await
            .map_err(|error| BenchError::Llm(error.to_string()))?;
        let runner = Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent))
            .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>)
            .run_config(RunConfig {
                history_max_events: config.history_max_events,
                ..RunConfig::default()
            })
            .build()
            .map_err(|error| BenchError::Llm(error.to_string()))?;
        Ok(Self { model, runner, sessions, turns: AtomicUsize::new(0) })
    }

    /// Runs one `Runner::run` to the end of its event stream and returns how many events it
    /// yielded.
    ///
    /// In [`LongHistoryMode::Turns`] that is one turn; in [`LongHistoryMode::Loop`] it is the
    /// whole autonomous run.
    ///
    /// # Errors
    ///
    /// Returns [`BenchError::Llm`] when the run fails.
    pub async fn turn(&self) -> Result<usize> {
        let turn = self.turns.fetch_add(1, Ordering::SeqCst);
        let mut stream = self
            .runner
            .run(
                UserId::new(USER).map_err(|error| BenchError::Llm(error.to_string()))?,
                SessionId::new(SESSION).map_err(|error| BenchError::Llm(error.to_string()))?,
                Content::new("user").with_text(format!("Turn {turn}: read the next file.")),
            )
            .await
            .map_err(|error| BenchError::Llm(error.to_string()))?;
        let mut events = 0;
        while let Some(event) = stream.next().await {
            event.map_err(|error| BenchError::Llm(error.to_string()))?;
            events += 1;
        }
        Ok(events)
    }

    /// Number of events stored in the session.
    ///
    /// # Errors
    ///
    /// Returns [`BenchError::Llm`] when the session cannot be read.
    pub async fn session_events(&self) -> Result<usize> {
        let session = self
            .sessions
            .get(GetRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: SESSION.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .map_err(|error| BenchError::Llm(error.to_string()))?;
        Ok(session.events().len())
    }
}

/// Runs `config` and measures every step.
///
/// # Errors
///
/// Returns [`BenchError::Llm`] when the agent, the runner or a run fails.
pub async fn run_long_history(config: &LongHistoryConfig) -> Result<LongHistoryReport> {
    let bench = LongHistoryBench::new(config).await?;
    let start_cpu = process_cpu_time();
    let mut step_wall = Vec::with_capacity(config.steps);
    let mut step_cpu = Vec::with_capacity(config.steps);
    match config.mode {
        LongHistoryMode::Turns => {
            for _ in 0..config.steps {
                let (wall, cpu) = (Instant::now(), process_cpu_time());
                bench.turn().await?;
                step_wall.push(wall.elapsed());
                if let (Some(before), Some(after)) = (cpu, process_cpu_time()) {
                    step_cpu.push(after.saturating_sub(before));
                }
            }
        }
        LongHistoryMode::Loop => {
            bench.turn().await?;
            // A step runs from one model call to the next: the tool call, the events it
            // appends, and the next request built from the grown conversation.
            let stamps =
                bench.model.stamps.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            for pair in stamps.windows(2) {
                step_wall.push(pair[1].0 - pair[0].0);
                if let (Some(before), Some(after)) = (pair[0].1, pair[1].1) {
                    step_cpu.push(after.saturating_sub(before));
                }
            }
        }
    }
    let total_cpu = start_cpu.zip(process_cpu_time()).map(|(start, end)| end.saturating_sub(start));
    let measured = if step_cpu.len() == step_wall.len() { &step_cpu } else { &step_wall };
    let tenth = (measured.len() / 10).max(1).min(measured.len());
    Ok(LongHistoryReport {
        mode: config.mode,
        session_events: bench.session_events().await?,
        first: compute_stats(&measured[..tenth]),
        last: compute_stats(&measured[measured.len() - tenth..]),
        step_wall,
        step_cpu,
        total_cpu,
        peak_rss_bytes: peak_rss_bytes(),
    })
}

/// CPU time of the whole process, all threads.
#[cfg(unix)]
fn process_cpu_time() -> Option<Duration> {
    let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `clock_gettime` writes one `timespec` through a valid, exclusive pointer.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &raw mut time) };
    (status == 0).then(|| {
        Duration::new(
            u64::try_from(time.tv_sec).unwrap_or(0),
            u32::try_from(time.tv_nsec).unwrap_or(0),
        )
    })
}

#[cfg(not(unix))]
fn process_cpu_time() -> Option<Duration> {
    None
}

/// Peak resident set size of the process so far.
#[cfg(unix)]
fn peak_rss_bytes() -> Option<u64> {
    // SAFETY: `rusage` is plain data, so all zeroes is a valid value, and `getrusage` writes
    // one through a valid, exclusive pointer.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) };
    let max_rss = u64::try_from(usage.ru_maxrss).ok().filter(|_| status == 0)?;
    // macOS reports bytes, Linux and the BSDs KiB.
    Some(if cfg!(target_os = "macos") { max_rss } else { max_rss * 1024 })
}

#[cfg(not(unix))]
fn peak_rss_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_turns_run_adds_four_events_a_turn() {
        let config = LongHistoryConfig { steps: 12, ..LongHistoryConfig::default() };
        let report = run_long_history(&config).await.unwrap();
        assert_eq!(
            (report.mode, report.session_events, report.step_wall.len(), report.first.count),
            (LongHistoryMode::Turns, 48, 12, 1)
        );
    }

    #[tokio::test]
    async fn a_loop_run_measures_every_tool_iteration() {
        let config = LongHistoryConfig {
            mode: LongHistoryMode::Loop,
            steps: 30,
            ..LongHistoryConfig::default()
        };
        let report = run_long_history(&config).await.unwrap();
        // The user message, 30 calls with their results, and the answer.
        assert_eq!(
            (report.mode, report.session_events, report.step_wall.len(), report.last.count),
            (LongHistoryMode::Loop, 62, 30, 3)
        );
    }
}
