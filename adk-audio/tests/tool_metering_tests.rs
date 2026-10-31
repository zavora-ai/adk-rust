//! `TranscribeTool` and `SpeakTool` meter a billed provider's calls against the run the
//! tool call belongs to; a local provider's calls are not metered.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_audio::{
    AudioBilling, AudioFrame, AudioResult, SpeakTool, SttOptions, SttProvider, TranscribeTool,
    Transcript, TtsProvider, TtsRequest, Voice,
};
use adk_core::{
    BudgetTracker, CallbackContext, Content, EventActions, InMemorySpendLedger, ModelCallScope,
    ReadonlyContext, RunBudget, RunConfig, SpendKey, SpendLedger, SpendLimits, SpendPeriod, Tool,
    ToolContext, UnitRate, UsageUnit, UsageVector,
};
use async_trait::async_trait;
use futures::Stream;
use serde_json::{Value, json};

/// A tool context that exposes a run configuration, as an agent's does.
struct RunContext {
    config: RunConfig,
    content: Content,
}

#[async_trait]
impl ReadonlyContext for RunContext {
    fn invocation_id(&self) -> &str {
        "invocation"
    }
    fn agent_name(&self) -> &str {
        "narrator"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn app_name(&self) -> &str {
        "studio"
    }
    fn session_id(&self) -> &str {
        "session"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for RunContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for RunContext {
    fn function_call_id(&self) -> &str {
        "call-1"
    }
    fn actions(&self) -> EventActions {
        EventActions::default()
    }
    fn set_actions(&self, _actions: EventActions) {}
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
        Ok(vec![])
    }
    fn run_config(&self) -> Option<&RunConfig> {
        Some(&self.config)
    }
}

struct Stt {
    billing: Option<AudioBilling>,
    calls: AtomicUsize,
}

#[async_trait]
impl SttProvider for Stt {
    async fn transcribe(&self, _audio: &AudioFrame, _opts: &SttOptions) -> AudioResult<Transcript> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Transcript { text: "hello".to_string(), confidence: 1.0, ..Default::default() })
    }

    async fn transcribe_stream(
        &self,
        _audio: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        _opts: &SttOptions,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<Transcript>> + Send>>> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn billing(&self) -> Option<AudioBilling> {
        self.billing.clone()
    }
}

struct Tts;

#[async_trait]
impl TtsProvider for Tts {
    async fn synthesize(&self, _request: &TtsRequest) -> AudioResult<AudioFrame> {
        Ok(AudioFrame::silence(16_000, 1, 100))
    }

    async fn synthesize_stream(
        &self,
        _request: &TtsRequest,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<AudioFrame>> + Send>>> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn voice_catalog(&self) -> &[Voice] {
        &[]
    }

    fn billing(&self) -> Option<AudioBilling> {
        Some(AudioBilling::new("speechco", "voice-1"))
    }
}

fn transcribe_args(seconds: usize) -> Value {
    // Silence at 16 kHz mono PCM16: 32,000 bytes per second, base64-encoded.
    let pcm = vec![0u8; seconds * 32_000];
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in pcm.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            encoded.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
        encoded.push_str(&"=".repeat(3 - chunk.len()));
    }
    json!({ "audio_data": encoded, "sample_rate": 16_000 })
}

fn context(config: RunConfig) -> Arc<dyn ToolContext> {
    Arc::new(RunContext { config, content: Content::new("user") })
}

fn transcription_billing() -> AudioBilling {
    AudioBilling::new("muse", "muse-voice-transcribe-1.0")
        .with_rate(UnitRate::new(UsageUnit::AudioMillis, 180_000, 3_600_000).rounded_down_to(1_000))
}

#[tokio::test]
async fn a_billed_transcription_commits_its_audio_to_the_run() {
    let ledger = Arc::new(InMemorySpendLedger::default());
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
    let mut config = RunConfig::builder().spend_ledger(ledger.clone()).build();
    config.budget_tracker = Some(tracker.clone());
    let ctx = context(config);
    let tool = TranscribeTool::new(Arc::new(Stt {
        billing: Some(transcription_billing()),
        calls: AtomicUsize::new(0),
    }));

    let result =
        ModelCallScope::within_tool(ctx.as_ref(), tool.execute(ctx.clone(), transcribe_args(20)))
            .await
            .unwrap();
    assert_eq!(result["text"], "hello");
    let muse = SpendKey::org("studio").with_vendor("muse");
    assert_eq!(ledger.spent(&muse, SpendPeriod::Day).await.unwrap(), 1_000);
    assert_eq!(tracker.units(), UsageVector::new().with(UsageUnit::AudioMillis, 20_000));
}

#[tokio::test]
async fn a_transcription_over_the_cap_is_never_sent() {
    let cap = SpendKey::org("studio").per(SpendPeriod::Day);
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new().limit(cap, 500)));
    let ctx = context(RunConfig::builder().spend_ledger(ledger).build());
    let stt = Arc::new(Stt { billing: Some(transcription_billing()), calls: AtomicUsize::new(0) });
    let tool = TranscribeTool::new(stt.clone());

    let refused =
        ModelCallScope::within_tool(ctx.as_ref(), tool.execute(ctx.clone(), transcribe_args(20)))
            .await
            .unwrap_err();
    assert_eq!(refused.code, adk_core::SPEND_LIMIT_EXCEEDED_CODE);
    assert_eq!(stt.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_unpriced_synthesis_is_refused_under_a_cost_cap_and_a_local_one_is_not_metered() {
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_cost_usd(1.0)));
    let mut config = RunConfig::builder().build();
    config.budget_tracker = Some(tracker.clone());
    let ctx = context(config);

    let speak = SpeakTool::new(Arc::new(Tts), "default");
    let refused = ModelCallScope::within_tool(
        ctx.as_ref(),
        speak.execute(ctx.clone(), json!({ "text": "hi" })),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.code, "budget.cost_unknown");

    let local = TranscribeTool::new(Arc::new(Stt { billing: None, calls: AtomicUsize::new(0) }));
    ModelCallScope::within_tool(ctx.as_ref(), local.execute(ctx.clone(), transcribe_args(1)))
        .await
        .unwrap();
    assert_eq!(tracker.units(), UsageVector::new());
}

#[tokio::test]
async fn synthesis_counts_the_characters_sent() {
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
    let mut config = RunConfig::builder().build();
    config.budget_tracker = Some(tracker.clone());
    let ctx = context(config);
    let speak = SpeakTool::new(Arc::new(Tts), "default");
    ModelCallScope::within_tool(
        ctx.as_ref(),
        speak.execute(ctx.clone(), json!({ "text": "héllo" })),
    )
    .await
    .unwrap();
    assert_eq!(tracker.units(), UsageVector::new().with(UsageUnit::Characters, 5));
}
