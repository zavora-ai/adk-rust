use adk_realtime::audio::{AudioChunk, AudioFormat};
use adk_realtime::error::Result;
use adk_realtime::events::{ClientEvent, ServerEvent, ToolResponse};
use adk_realtime::model::RealtimeModel;
use adk_realtime::runner::RealtimeRunner;
use adk_realtime::session::{ContextMutationOutcome, RealtimeSession};
use async_trait::async_trait;
use futures::Stream;
use std::hint::black_box;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

// ── Simple Allocation Counter ───────────────────────────────────────────

use std::alloc::{GlobalAlloc, Layout, System};

struct AllocCounter;

static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);
static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for AllocCounter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::SeqCst);
        ALLOC_BYTES.fetch_add(layout.size(), Ordering::SeqCst);
        // SAFETY: Delegating to System allocator
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: Delegating to System allocator
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: AllocCounter = AllocCounter;

// ── Mock Session & Model ───────────────────────────────────────────────

struct BenchSession;
#[async_trait]
impl RealtimeSession for BenchSession {
    fn session_id(&self) -> &str {
        "bench"
    }
    fn is_connected(&self) -> bool {
        true
    }
    async fn send_audio(&self, audio: &AudioChunk) -> Result<()> {
        black_box(audio);
        Ok(())
    }
    async fn send_audio_base64(&self, audio_base64: &str) -> Result<()> {
        black_box(audio_base64);
        Ok(())
    }
    async fn send_text(&self, _text: &str) -> Result<()> {
        Ok(())
    }
    async fn send_tool_response(&self, _response: ToolResponse) -> Result<()> {
        Ok(())
    }
    async fn commit_audio(&self) -> Result<()> {
        Ok(())
    }
    async fn clear_audio(&self) -> Result<()> {
        Ok(())
    }
    async fn create_response(&self) -> Result<()> {
        Ok(())
    }
    async fn interrupt(&self) -> Result<()> {
        Ok(())
    }
    async fn send_event(&self, _event: ClientEvent) -> Result<()> {
        Ok(())
    }
    async fn next_event(&self) -> Option<Result<ServerEvent>> {
        None
    }
    fn events(&self) -> Pin<Box<dyn Stream<Item = Result<ServerEvent>> + Send + '_>> {
        Box::pin(futures::stream::empty())
    }
    async fn close(&self) -> Result<()> {
        Ok(())
    }
    async fn mutate_context(
        &self,
        _config: adk_realtime::config::RealtimeConfig,
    ) -> Result<ContextMutationOutcome> {
        Ok(ContextMutationOutcome::Applied)
    }
}

struct BenchModel;
#[async_trait]
impl RealtimeModel for BenchModel {
    fn provider(&self) -> &str {
        "bench"
    }
    fn model_id(&self) -> &str {
        "bench"
    }
    fn supported_input_formats(&self) -> Vec<AudioFormat> {
        vec![]
    }
    fn supported_output_formats(&self) -> Vec<AudioFormat> {
        vec![]
    }
    fn available_voices(&self) -> Vec<&str> {
        vec![]
    }
    async fn connect(
        &self,
        _config: adk_realtime::config::RealtimeConfig,
    ) -> Result<Box<dyn RealtimeSession>> {
        Ok(Box::new(BenchSession))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let runner = RealtimeRunner::builder().model(Arc::new(BenchModel)).build()?;
    runner.connect().await?;
    let runner = Arc::new(runner);

    println!("--- Audio Boundary Benchmark: Baseline vs Optimized ---");
    println!("Architecture: {} {}", std::env::consts::OS, std::env::consts::ARCH);
    #[cfg(debug_assertions)]
    println!("Build Profile: Debug (Note: results will be slower)");
    #[cfg(not(debug_assertions))]
    println!("Build Profile: Release");

    let sample_rates = [16000, 24000];
    let durations_ms = [10, 20, 40, 80];

    for &rate in &sample_rates {
        for &ms in &durations_ms {
            run_comparison("PCM16", runner.clone(), rate, ms, false).await?;
        }
    }

    // Realistic Twilio Case: 20ms of G.711 mulaw at 8kHz
    run_comparison("G.711 mulaw (Twilio)", runner.clone(), 8000, 20, true).await?;

    Ok(())
}

async fn run_comparison(
    label: &str,
    runner: Arc<RealtimeRunner>,
    sample_rate: u32,
    duration_ms: u32,
    is_ulaw: bool,
) -> Result<()> {
    let format = if is_ulaw {
        AudioFormat::g711_ulaw()
    } else if sample_rate == 16000 {
        AudioFormat::pcm16_16khz()
    } else {
        AudioFormat::pcm16_24khz()
    };

    let bytes_count = (format.bytes_per_second() as f64 * duration_ms as f64 / 1000.0) as usize;
    let data = vec![0u8; bytes_count];
    let chunk = AudioChunk::new(data, format);

    let iterations = 10000;

    // Reset counters
    ALLOC_COUNT.store(0, Ordering::SeqCst);
    ALLOC_BYTES.store(0, Ordering::SeqCst);

    // Baseline: Bridge (simulated) -> to_base64 -> runner.send_audio_base64
    let start = Instant::now();
    for _ in 0..iterations {
        let b64 = chunk.to_base64();
        runner.send_audio_base64(&b64).await?;
    }
    let baseline_elapsed = start.elapsed();
    let baseline_allocs = ALLOC_COUNT.load(Ordering::SeqCst);
    let baseline_bytes = ALLOC_BYTES.load(Ordering::SeqCst);

    // Reset counters
    ALLOC_COUNT.store(0, Ordering::SeqCst);
    ALLOC_BYTES.store(0, Ordering::SeqCst);

    // Optimized: Bridge (simulated) -> runner.send_audio_chunk
    let start = Instant::now();
    for _ in 0..iterations {
        runner.send_audio_chunk(&chunk).await?;
    }
    let optimized_elapsed = start.elapsed();
    let optimized_allocs = ALLOC_COUNT.load(Ordering::SeqCst);
    let optimized_bytes = ALLOC_BYTES.load(Ordering::SeqCst);

    println!(
        "\nType: {}, Rate: {}Hz, Dur: {}ms | Chunk size: {} bytes",
        label,
        sample_rate,
        duration_ms,
        chunk.data.len()
    );
    println!(
        "  Baseline (base64):  {:>10?} | Chunks/sec: {:>10.0} | Allocs: {:>8} | Bytes: {:>10}",
        baseline_elapsed / iterations,
        iterations as f64 / baseline_elapsed.as_secs_f64(),
        baseline_allocs,
        baseline_bytes
    );
    println!(
        "  Optimized (raw):    {:>10?} | Chunks/sec: {:>10.0} | Allocs: {:>8} | Bytes: {:>10}",
        optimized_elapsed / iterations,
        iterations as f64 / optimized_elapsed.as_secs_f64(),
        optimized_allocs,
        optimized_bytes
    );
    let speedup = baseline_elapsed.as_secs_f64() / optimized_elapsed.as_secs_f64();
    println!("  Speedup:            {:.2}x", speedup);

    Ok(())
}
