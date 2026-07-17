mod support;

use std::hint::black_box;
use std::time::Instant;

use adk_realtime::audio::AudioFormat;
use anyhow::{Context, Result, anyhow, ensure};
use support::{BenchChunk, BytesChunk, FANOUT, FRAME_CASES, FrameCase, VecChunk};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const WARMUP_FRAMES: usize = 2_000;
const MEASURED_FRAMES: usize = 20_000;

#[derive(Debug)]
struct LatencyReport {
    scenario: &'static str,
    payload: &'static str,
    frame: &'static str,
    bytes: usize,
    p50_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    mean_ns: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct Observation {
    len: usize,
    first: Option<u8>,
    last: Option<u8>,
    format: AudioFormat,
}

fn observe<C: BenchChunk>(chunk: &C) -> Observation {
    Observation {
        len: chunk.data().len(),
        first: chunk.data().first().copied(),
        last: chunk.data().last().copied(),
        format: chunk.format().clone(),
    }
}

impl LatencyReport {
    fn from_samples<C: BenchChunk>(
        scenario: &'static str,
        frame: FrameCase,
        mut samples: Vec<u64>,
    ) -> Self {
        samples.sort_unstable();
        let sum = samples.iter().map(|sample| u128::from(*sample)).sum::<u128>();
        let mean_ns = (sum / samples.len() as u128) as u64;

        Self {
            scenario,
            payload: C::LABEL,
            frame: frame.name,
            bytes: frame.bytes,
            p50_ns: percentile(&samples, 50),
            p95_ns: percentile(&samples, 95),
            p99_ns: percentile(&samples, 99),
            mean_ns,
        }
    }

    fn frames_per_second(&self) -> f64 {
        1_000_000_000.0 / self.mean_ns as f64
    }

    fn mebibytes_per_second(&self) -> f64 {
        self.frames_per_second() * self.bytes as f64 / (1024.0 * 1024.0)
    }
}

fn percentile(samples: &[u64], percentile: usize) -> u64 {
    let index = (samples.len() - 1) * percentile / 100;
    samples[index]
}

fn timer_floor_ns() -> u64 {
    let mut samples = Vec::with_capacity(MEASURED_FRAMES);
    for _ in 0..MEASURED_FRAMES {
        let start = Instant::now();
        black_box(());
        samples.push(start.elapsed().as_nanos() as u64);
    }
    samples.sort_unstable();
    percentile(&samples, 50)
}

fn spawn_consumer<C: BenchChunk>(
    mut receiver: mpsc::Receiver<C>,
    acknowledgements: mpsc::Sender<Observation>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(chunk) = receiver.recv().await {
            let observation = observe(black_box(&chunk));
            if acknowledgements.send(observation).await.is_err() {
                break;
            }
        }
    })
}

async fn measure_cross_task_move<C: BenchChunk>(frame: FrameCase) -> Result<LatencyReport> {
    let (sender, receiver) = mpsc::channel(1);
    let (acknowledgements, mut observed) = mpsc::channel(1);
    let consumer = spawn_consumer::<C>(receiver, acknowledgements);
    let mut samples = Vec::with_capacity(MEASURED_FRAMES);

    for iteration in 0..(WARMUP_FRAMES + MEASURED_FRAMES) {
        let chunk = C::from_owned(frame.payload(), frame.format());
        let expected = observe(&chunk);
        let start = Instant::now();
        sender.send(chunk).await.map_err(|_| anyhow!("cross-task consumer closed"))?;
        let actual = observed.recv().await.context("cross-task acknowledgement closed")?;
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        ensure!(actual == expected, "cross-task payload changed");

        if iteration >= WARMUP_FRAMES {
            samples.push(elapsed_ns);
        }
    }

    drop(sender);
    consumer.await.context("cross-task consumer panicked")?;
    Ok(LatencyReport::from_samples::<C>("cross_task_move", frame, samples))
}

async fn measure_cross_task_fanout<C: BenchChunk>(frame: FrameCase) -> Result<LatencyReport> {
    let (acknowledgements, mut observed) = mpsc::channel(FANOUT);
    let mut senders = Vec::with_capacity(FANOUT);
    let mut consumers = Vec::with_capacity(FANOUT);

    for _ in 0..FANOUT {
        let (sender, receiver) = mpsc::channel(1);
        senders.push(sender);
        consumers.push(spawn_consumer::<C>(receiver, acknowledgements.clone()));
    }
    drop(acknowledgements);

    let mut samples = Vec::with_capacity(MEASURED_FRAMES);
    for iteration in 0..(WARMUP_FRAMES + MEASURED_FRAMES) {
        let chunk = C::from_owned(frame.payload(), frame.format());
        let expected = observe(&chunk);
        let start = Instant::now();
        let payloads = [chunk.clone(), chunk.clone(), chunk.clone(), chunk];

        for (sender, payload) in senders.iter().zip(payloads) {
            sender.send(payload).await.map_err(|_| anyhow!("fan-out consumer closed"))?;
        }
        for _ in 0..FANOUT {
            let actual = observed.recv().await.context("fan-out acknowledgement closed")?;
            ensure!(actual == expected, "fan-out payload changed");
        }

        let elapsed_ns = start.elapsed().as_nanos() as u64;
        if iteration >= WARMUP_FRAMES {
            samples.push(elapsed_ns);
        }
    }

    drop(senders);
    for consumer in consumers {
        consumer.await.context("fan-out consumer panicked")?;
    }
    Ok(LatencyReport::from_samples::<C>("cross_task_fanout_four", frame, samples))
}

fn print_report(reports: &[LatencyReport]) {
    println!("timer_floor_p50_ns={}", timer_floor_ns());
    println!(
        "scenario,payload,frame,bytes,p50_ns,p95_ns,p99_ns,mean_ns,frames_per_second,mib_per_second"
    );
    for report in reports {
        println!(
            "{},{},{},{},{},{},{},{},{:.0},{:.2}",
            report.scenario,
            report.payload,
            report.frame,
            report.bytes,
            report.p50_ns,
            report.p95_ns,
            report.p99_ns,
            report.mean_ns,
            report.frames_per_second(),
            report.mebibytes_per_second(),
        );
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    println!("audio_payload_tail_latency");
    println!("os={},arch={}", std::env::consts::OS, std::env::consts::ARCH);
    println!("warmup_frames={WARMUP_FRAMES},measured_frames={MEASURED_FRAMES},channel_capacity=1");

    let mut reports = Vec::new();
    for frame in FRAME_CASES {
        support::validate_representations(frame);
        reports.push(measure_cross_task_move::<VecChunk>(frame).await?);
        reports.push(measure_cross_task_move::<BytesChunk>(frame).await?);
        reports.push(measure_cross_task_fanout::<VecChunk>(frame).await?);
        reports.push(measure_cross_task_fanout::<BytesChunk>(frame).await?);
    }
    print_report(&reports);
    Ok(())
}
