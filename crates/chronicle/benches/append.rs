use criterion::{black_box, BatchSize, BenchmarkId, Criterion};
use criterion::{criterion_group, criterion_main};
use tempfile::tempdir;

use chronicle::core::Queue;

const APPENDS_PER_ITER: usize = 10_000;

fn bench_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("append");
    for &size in &[64_usize, 256, 1024] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter_batched(
                || {
                    let dir = tempdir().expect("tempdir");
                    let path = dir.path().join("bench_queue");
                    let writer = Queue::open_publisher(&path).expect("writer");
                    let payload = vec![0u8; size];
                    (dir, writer, payload)
                },
                |(_dir, mut writer, payload)| {
                    for _ in 0..APPENDS_PER_ITER {
                        writer.append(1, black_box(&payload)).expect("append");
                    }
                    writer.flush_sync().expect("flush");
                },
                BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_append);
criterion_main!(benches);
