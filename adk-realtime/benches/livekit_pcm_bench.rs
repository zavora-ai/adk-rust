use std::borrow::Cow;
use std::hint::black_box;
use std::time::{Duration, Instant};

fn main() {
    let bytes: Vec<u8> = vec![0; 10_000_000];
    let iterations = 1000;
    let warmup = 100;

    let mut manual_durations = Vec::with_capacity(iterations);
    let mut iter_durations = Vec::new();
    let mut bytemuck_durations = Vec::new();

    let mut samples1 = Vec::new();
    let mut samples2 = Vec::new();

    // Warm-up
    for _ in 0..warmup {
        let mut s = Vec::with_capacity(bytes.len() / 2);
        for chunk in bytes.chunks_exact(2) {
            s.push(i16::from_le_bytes([chunk[0], chunk[1]]));
        }
        black_box(s);

        let s: Vec<i16> = bytes.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        black_box(s);

        let s: Cow<[i16]> = unsafe {
            Cow::Borrowed(std::slice::from_raw_parts(bytes.as_ptr() as *const i16, bytes.len() / 2))
        };
        black_box(s);
    }

    // Benchmark
    for _ in 0..iterations {
        let start = Instant::now();
        let mut s = Vec::with_capacity(bytes.len() / 2);
        for chunk in bytes.chunks_exact(2) {
            s.push(i16::from_le_bytes([chunk[0], chunk[1]]));
        }
        manual_durations.push(start.elapsed());
        samples1 = s;

        let start = Instant::now();
        let s: Vec<i16> = bytes.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        iter_durations.push(start.elapsed());
        samples2 = s;

        let start = Instant::now();
        let s: Cow<[i16]> = unsafe {
            Cow::Borrowed(std::slice::from_raw_parts(bytes.as_ptr() as *const i16, bytes.len() / 2))
        };
        black_box(s);
        bytemuck_durations.push(start.elapsed());
    }

    assert_eq!(samples1, samples2, "Fatal: pcm16 outputs differ!");

    print_stats("Manual Loop", &mut manual_durations);
    print_stats("Iterator / Collect", &mut iter_durations);
    print_stats("Absolute Zero-Copy (bytemuck simulated)", &mut bytemuck_durations);
}

fn print_stats(name: &str, durations: &mut [Duration]) {
    durations.sort_unstable();
    let count = durations.len();
    let sum: Duration = durations.iter().sum();
    let mean = sum / count as u32;

    let median = if count % 2 == 0 {
        (durations[count / 2 - 1] + durations[count / 2]) / 2
    } else {
        durations[count / 2]
    };

    let mean_f64 = mean.as_secs_f64();
    let variance = durations
        .iter()
        .map(|d| {
            let diff = d.as_secs_f64() - mean_f64;
            diff * diff
        })
        .sum::<f64>()
        / count as f64;
    let stddev = Duration::from_secs_f64(variance.sqrt());

    println!("=== {} ===", name);
    println!("Mean:   {:?}", mean);
    println!("Median: {:?}", median);
    println!("StdDev: {:?}", stddev);
    println!("Min:    {:?}", durations[0]);
    println!("Max:    {:?}", durations[count - 1]);
    println!();
}
