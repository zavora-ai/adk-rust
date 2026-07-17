mod support;

use std::alloc::System;
use std::hint::black_box;

use stats_alloc::{INSTRUMENTED_SYSTEM, Region, Stats, StatsAlloc};
use support::{BenchChunk, BytesChunk, FANOUT, FRAME_CASES, FrameCase, VecChunk};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const WARMUP_OPERATIONS: usize = 1_000;
const MEASURED_OPERATIONS: usize = 10_000;

#[derive(Debug)]
struct AllocationReport {
    scenario: &'static str,
    payload: &'static str,
    frame: &'static str,
    bytes: usize,
    stats: Stats,
}

impl AllocationReport {
    fn allocations_per_operation(&self) -> f64 {
        self.stats.allocations as f64 / MEASURED_OPERATIONS as f64
    }

    fn allocated_bytes_per_operation(&self) -> f64 {
        self.stats.bytes_allocated as f64 / MEASURED_OPERATIONS as f64
    }

    fn reallocations_per_operation(&self) -> f64 {
        self.stats.reallocations as f64 / MEASURED_OPERATIONS as f64
    }
}

fn measure<C, F>(scenario: &'static str, frame: FrameCase, mut operation: F) -> AllocationReport
where
    C: BenchChunk,
    F: FnMut(),
{
    for _ in 0..WARMUP_OPERATIONS {
        operation();
    }

    let region = Region::new(GLOBAL);
    for _ in 0..MEASURED_OPERATIONS {
        operation();
    }
    let stats = region.change();

    AllocationReport { scenario, payload: C::LABEL, frame: frame.name, bytes: frame.bytes, stats }
}

fn owned<C: BenchChunk>(frame: FrameCase) -> AllocationReport {
    measure::<C, _>("owned_vec_input", frame, || {
        let chunk = C::from_owned(frame.payload(), frame.format());
        black_box(chunk);
    })
}

fn borrowed<C: BenchChunk>(frame: FrameCase) -> AllocationReport {
    let payload = frame.payload();
    measure::<C, _>("borrowed_input", frame, || {
        let chunk = C::from_borrowed(black_box(payload.as_slice()), frame.format());
        black_box(chunk);
    })
}

fn clone_one<C: BenchChunk>(frame: FrameCase) -> AllocationReport {
    let chunk = C::from_owned(frame.payload(), frame.format());
    measure::<C, _>("clone_one", frame, || {
        let copy = black_box(&chunk).clone();
        black_box(copy);
    })
}

fn fanout_four<C: BenchChunk>(frame: FrameCase) -> AllocationReport {
    let chunk = C::from_owned(frame.payload(), frame.format());
    measure::<C, _>("fanout_four", frame, || {
        let copies = std::array::from_fn::<_, FANOUT, _>(|_| black_box(&chunk).clone());
        black_box(copies);
    })
}

fn live_fanout_heap<C: BenchChunk>(frame: FrameCase) -> usize {
    let chunk = C::from_owned(frame.payload(), frame.format());
    let region = Region::new(GLOBAL);
    let copies = std::array::from_fn::<_, FANOUT, _>(|_| black_box(&chunk).clone());
    let stats = region.change();
    black_box(&copies);

    stats.bytes_allocated.saturating_sub(stats.bytes_deallocated)
}

fn print_reports(reports: &[AllocationReport]) {
    println!(
        "scenario,payload,frame,bytes,allocations_per_op,reallocations_per_op,allocated_bytes_per_op"
    );
    for report in reports {
        println!(
            "{},{},{},{},{:.4},{:.4},{:.2}",
            report.scenario,
            report.payload,
            report.frame,
            report.bytes,
            report.allocations_per_operation(),
            report.reallocations_per_operation(),
            report.allocated_bytes_per_operation(),
        );
    }
}

fn main() {
    println!("audio_payload_allocations");
    println!("os={},arch={}", std::env::consts::OS, std::env::consts::ARCH);
    println!("warmup_operations={WARMUP_OPERATIONS},measured_operations={MEASURED_OPERATIONS}");

    let mut reports = Vec::new();
    for frame in FRAME_CASES {
        support::validate_representations(frame);
        reports.push(owned::<VecChunk>(frame));
        reports.push(owned::<BytesChunk>(frame));
        reports.push(borrowed::<VecChunk>(frame));
        reports.push(borrowed::<BytesChunk>(frame));
        reports.push(clone_one::<VecChunk>(frame));
        reports.push(clone_one::<BytesChunk>(frame));
        reports.push(fanout_four::<VecChunk>(frame));
        reports.push(fanout_four::<BytesChunk>(frame));
    }
    print_reports(&reports);

    println!("live_fanout_heap_bytes,payload,frame,bytes,live_heap_bytes");
    for frame in FRAME_CASES {
        println!(
            "live_fanout_heap_bytes,{},{},{},{}",
            VecChunk::LABEL,
            frame.name,
            frame.bytes,
            live_fanout_heap::<VecChunk>(frame),
        );
        println!(
            "live_fanout_heap_bytes,{},{},{},{}",
            BytesChunk::LABEL,
            frame.name,
            frame.bytes,
            live_fanout_heap::<BytesChunk>(frame),
        );
    }
}
