use adk_realtime::audio::{AudioChunk, AudioFormat};
use std::alloc::{GlobalAlloc, Layout, System};
use std::borrow::Cow;
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct MemoryTracker;

static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);
static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for MemoryTracker {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: MemoryTracker = MemoryTracker;

fn old_to_i16_samples(data: &[u8]) -> Result<Vec<i16>, String> {
    if !data.len().is_multiple_of(2) {
        return Err(format!("Invalid data length: {}", data.len()));
    }
    let mut samples = Vec::with_capacity(data.len() / 2);
    for chunk in data.chunks_exact(2) {
        samples.push(i16::from_le_bytes([chunk[0], chunk[1]]));
    }
    Ok(samples)
}

fn percentile(sorted: &[u128], pct: f64) -> u128 {
    let idx = ((sorted.len() as f64 - 1.0) * pct).round() as usize;
    sorted[idx]
}

fn main() {
    println!("\n⚡ Zenith ADK-Rust Audio Boundary Performance Benchmark ⚡");
    println!("------------------------------------------------------------");

    let iterations = 1_000_000;
    // 20ms of 24kHz PCM16 audio = 480 samples = 960 bytes (typical frame)
    let sample_count = 480;
    let samples: Vec<i16> = (0..sample_count).map(|i| (i * 37) as i16).collect();
    let chunk = AudioChunk::from_i16_samples(&samples, AudioFormat::pcm16_24khz());

    // ── 1. Benchmark Old Allocation Method ──
    ALLOC_COUNT.store(0, Ordering::SeqCst);
    ALLOC_BYTES.store(0, Ordering::SeqCst);

    let mut latencies_old = Vec::with_capacity(iterations);
    let start_total_old = Instant::now();

    for _ in 0..iterations {
        let t0 = Instant::now();
        let res = old_to_i16_samples(black_box(&chunk.data)).unwrap();
        black_box(&res);
        latencies_old.push(t0.elapsed().as_nanos());
    }
    let total_time_old = start_total_old.elapsed();
    let allocs_old = ALLOC_COUNT.load(Ordering::SeqCst);
    let bytes_old = ALLOC_BYTES.load(Ordering::SeqCst);

    latencies_old.sort_unstable();

    // ── 2. Benchmark New Zero-Copy Cow Method ──
    ALLOC_COUNT.store(0, Ordering::SeqCst);
    ALLOC_BYTES.store(0, Ordering::SeqCst);

    let mut latencies_new = Vec::with_capacity(iterations);
    let start_total_new = Instant::now();

    for _ in 0..iterations {
        let t0 = Instant::now();
        let cow_samples = black_box(&chunk).to_i16_samples().unwrap();
        match &cow_samples {
            Cow::Borrowed(s) => black_box(*s),
            Cow::Owned(v) => black_box(v.as_slice()),
        };
        latencies_new.push(t0.elapsed().as_nanos());
    }
    let total_time_new = start_total_new.elapsed();
    let allocs_new = ALLOC_COUNT.load(Ordering::SeqCst);
    let bytes_new = ALLOC_BYTES.load(Ordering::SeqCst);

    latencies_new.sort_unstable();

    // ── Metrics Calculation ──
    let mean_ns_old = latencies_old.iter().sum::<u128>() as f64 / iterations as f64;
    let mean_ns_new = latencies_new.iter().sum::<u128>() as f64 / iterations as f64;

    let p50_old = percentile(&latencies_old, 0.50);
    let p50_new = percentile(&latencies_new, 0.50);

    let p95_old = percentile(&latencies_old, 0.95);
    let p95_new = percentile(&latencies_new, 0.95);

    let p99_old = percentile(&latencies_old, 0.99);
    let p99_new = percentile(&latencies_new, 0.99);

    let speedup_mean = mean_ns_old / mean_ns_new;
    let alloc_reduction = if allocs_old > 0 {
        ((allocs_old - allocs_new) as f64 / allocs_old as f64) * 100.0
    } else {
        0.0
    };

    println!("\n📊 BENCHMARK RESULTS ({} iterations)", iterations);
    println!("Frame Payload: 20ms @ 24kHz PCM16 (960 bytes, 480 samples)");
    println!("------------------------------------------------------------");
    println!(" Metric                │ Old (Vec<i16>)    │ New (Zero-Copy Cow) │ Improvement");
    println!("───────────────────────┼───────────────────┼─────────────────────┼──────────────");
    println!(
        " Total Allocations     │ {:>17} │ {:>19} │ -{:.1}%",
        allocs_old, allocs_new, alloc_reduction
    );
    println!(
        " Total Memory Allocated│ {:>14} B │ {:>16} B │ -{:.1}%",
        bytes_old,
        bytes_new,
        if bytes_old > 0 {
            ((bytes_old - bytes_new) as f64 / bytes_old as f64) * 100.0
        } else {
            0.0
        }
    );
    println!(
        " Mean Latency          │ {:>14.2} ns│ {:>16.2} ns│ {:.2}x faster",
        mean_ns_old, mean_ns_new, speedup_mean
    );
    println!(
        " Median (P50) Latency  │ {:>14} ns│ {:>16} ns│ {:.2}x faster",
        p50_old,
        p50_new,
        p50_old as f64 / p50_new.max(1) as f64
    );
    println!(
        " P95 Latency           │ {:>14} ns│ {:>16} ns│ {:.2}x faster",
        p95_old,
        p95_new,
        p95_old as f64 / p95_new.max(1) as f64
    );
    println!(
        " P99 Latency           │ {:>14} ns│ {:>16} ns│ {:.2}x faster",
        p99_old,
        p99_new,
        p99_old as f64 / p99_new.max(1) as f64
    );
    println!(
        " Total Wall-Clock Time │ {:>14.2} ms│ {:>16.2} ms│ {:.2}x faster",
        total_time_old.as_secs_f64() * 1000.0,
        total_time_new.as_secs_f64() * 1000.0,
        total_time_old.as_secs_f64() / total_time_new.as_secs_f64()
    );
    println!("------------------------------------------------------------\n");
}
