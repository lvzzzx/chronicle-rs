use std::sync::atomic::{AtomicUsize, Ordering};

use criterion::{black_box, BatchSize, Criterion};
use criterion::{criterion_group, criterion_main};
use tempfile::tempdir;

use chronicle_core::{Queue, ReaderConfig, StartMode, WriterConfig};

const SEGMENT_SIZE_BYTES: u64 = 8 * 1024 * 1024;
const RECORDS: u64 = 150_000;
const TARGET_SEQ: u64 = 65_000;
const BASE_TS_NS: u64 = 1_000_000_000;

fn bench_seek(c: &mut Criterion) {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("bench_queue");
    let config = WriterConfig {
        segment_size_bytes: SEGMENT_SIZE_BYTES,
        ..WriterConfig::default()
    };
    let mut writer = Queue::open_publisher_with_config(&path, config).expect("writer");
    for i in 0..RECORDS {
        writer
            .append_with_timestamp(1, &[], BASE_TS_NS + i)
            .expect("append");
    }
    writer.flush_sync().expect("flush");
    drop(writer);

    let mut group = c.benchmark_group("seek");
    let counter = AtomicUsize::new(0);
    let reader_config = ReaderConfig {
        start_mode: StartMode::Earliest,
        memlock: false,
    };

    group.bench_function("seek_seq_mid", |b| {
        b.iter_batched(
            || {
                let id = counter.fetch_add(1, Ordering::Relaxed);
                let name = format!("bench_reader_seq_{id}");
                Queue::open_subscriber_with_config(&path, &name, reader_config)
                    .expect("reader")
            },
            |mut reader| {
                reader.seek_seq(TARGET_SEQ).expect("seek");
                let msg = reader.next().expect("next").expect("msg");
                black_box(msg.seq);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("seek_timestamp_mid", |b| {
        b.iter_batched(
            || {
                let id = counter.fetch_add(1, Ordering::Relaxed);
                let name = format!("bench_reader_ts_{id}");
                Queue::open_subscriber_with_config(&path, &name, reader_config)
                    .expect("reader")
            },
            |mut reader| {
                reader
                    .seek_timestamp(BASE_TS_NS + TARGET_SEQ)
                    .expect("seek");
                let msg = reader.next().expect("next").expect("msg");
                black_box(msg.seq);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_seek);
criterion_main!(benches);
