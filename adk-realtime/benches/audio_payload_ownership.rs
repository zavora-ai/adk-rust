mod support;

use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use support::{BenchChunk, BytesChunk, FANOUT, FRAME_CASES, FrameCase, VecChunk};

fn bench_owned<C: BenchChunk>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    frame: FrameCase,
) {
    group.bench_with_input(BenchmarkId::new(C::LABEL, frame.name), &frame, |bencher, frame| {
        bencher.iter_batched(
            || frame.payload(),
            |data| black_box(C::from_owned(data, frame.format())),
            BatchSize::SmallInput,
        );
    });
}

fn bench_borrowed<C: BenchChunk>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    frame: FrameCase,
) {
    let payload = frame.payload();
    group.bench_with_input(BenchmarkId::new(C::LABEL, frame.name), &frame, |bencher, frame| {
        bencher.iter(|| C::from_borrowed(black_box(payload.as_slice()), frame.format()));
    });
}

fn bench_clone<C: BenchChunk>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    frame: FrameCase,
) {
    let chunk = C::from_owned(frame.payload(), frame.format());
    group.bench_with_input(BenchmarkId::new(C::LABEL, frame.name), &frame, |bencher, _| {
        bencher.iter(|| black_box(&chunk).clone());
    });
}

fn bench_fanout<C: BenchChunk>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    frame: FrameCase,
) {
    let chunk = C::from_owned(frame.payload(), frame.format());
    group.bench_with_input(BenchmarkId::new(C::LABEL, frame.name), &frame, |bencher, _| {
        bencher.iter(|| {
            let copies = std::array::from_fn::<_, FANOUT, _>(|_| black_box(&chunk).clone());
            black_box(copies)
        });
    });
}

fn audio_payload_ownership(criterion: &mut Criterion) {
    for frame in FRAME_CASES {
        support::validate_representations(frame);
    }

    {
        let mut group = criterion.benchmark_group("audio_payload/owned_vec_input");
        for frame in FRAME_CASES {
            group.throughput(Throughput::Bytes(frame.bytes as u64));
            bench_owned::<VecChunk>(&mut group, frame);
            bench_owned::<BytesChunk>(&mut group, frame);
        }
        group.finish();
    }

    {
        let mut group = criterion.benchmark_group("audio_payload/borrowed_input");
        for frame in FRAME_CASES {
            group.throughput(Throughput::Bytes(frame.bytes as u64));
            bench_borrowed::<VecChunk>(&mut group, frame);
            bench_borrowed::<BytesChunk>(&mut group, frame);
        }
        group.finish();
    }

    {
        let mut group = criterion.benchmark_group("audio_payload/clone_one");
        for frame in FRAME_CASES {
            group.throughput(Throughput::Bytes(frame.bytes as u64));
            bench_clone::<VecChunk>(&mut group, frame);
            bench_clone::<BytesChunk>(&mut group, frame);
        }
        group.finish();
    }

    {
        let mut group = criterion.benchmark_group("audio_payload/fanout_four");
        for frame in FRAME_CASES {
            group.throughput(Throughput::Bytes(frame.bytes as u64));
            bench_fanout::<VecChunk>(&mut group, frame);
            bench_fanout::<BytesChunk>(&mut group, frame);
        }
        group.finish();
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .sample_size(100);
    targets = audio_payload_ownership
}
criterion_main!(benches);
