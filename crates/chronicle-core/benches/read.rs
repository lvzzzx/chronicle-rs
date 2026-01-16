use criterion::{black_box, BatchSize, BenchmarkId, Criterion};
use criterion::{criterion_group, criterion_main};
use tempfile::tempdir;

use chronicle_core::Queue;

const APPENDS_PER_ITER: usize = 10_000;

fn bench_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("read");
    for &size in &[64_usize, 256, 1024] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter_batched(
                || {
                    let dir = tempdir().expect("tempdir");
                    let path = dir.path().join("bench_queue");
                    let mut writer = Queue::open_publisher(&path).expect("writer");
                    let payload = vec![0u8; size];
                    for _ in 0..APPENDS_PER_ITER {
                        writer.append(1, &payload).expect("append");
                    }
                    writer.flush_sync().expect("flush");
                    let reader = Queue::open_subscriber(&path, "bench").expect("reader");
                    (dir, reader)
                },
                |(_dir, mut reader)| {
                    let mut count = 0usize;
                    while let Some(message) = reader.next().expect("next") {
                        black_box(message.payload);
                        count += 1;
                    }
                    assert_eq!(count, APPENDS_PER_ITER);
                },
                BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_read);
criterion_main!(benches);
