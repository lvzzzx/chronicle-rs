use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;

use chronicle::stream::merge::FanInReader;
use chronicle::core::reader::WaitStrategy;
use chronicle::core::Queue;
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use tempfile::tempdir;

const MSG_SIZE: usize = 256;
const WRITERS: usize = 4;
const MSGS_PER_WRITER: usize = 100_000;
const TOTAL_MSGS: usize = WRITERS * MSGS_PER_WRITER;

fn bench_fanin_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("fanin_throughput");
    group.throughput(Throughput::Elements(TOTAL_MSGS as u64));

    group.bench_function(format!("{}_writers_1_reader", WRITERS), |b| {
        b.iter_custom(|iters| {
            let dir = tempdir().expect("tempdir");
            let mut readers = Vec::with_capacity(WRITERS);
            let mut paths = Vec::with_capacity(WRITERS);

            for i in 0..WRITERS {
                let path = dir.path().join(format!("q_{}", i));
                // Just create them, writers will open them
                let _writer = Queue::open_publisher(&path).expect("writer create");
                let reader = Queue::open_subscriber(&path, "fanin_bench").expect("reader open");
                readers.push(reader);
                paths.push(path);
            }

            let mut fanin = FanInReader::new(readers);
            fanin.set_wait_strategy(WaitStrategy::BusySpin);

            let payload = vec![0u8; MSG_SIZE];
            let barrier = Arc::new(Barrier::new(WRITERS + 1));

            let total_msgs_to_read = TOTAL_MSGS as u64 * iters;
            let msgs_per_writer_iter = MSGS_PER_WRITER as u64 * iters;

            let mut writer_handles = Vec::new();
            for path in paths {
                let barrier_clone = barrier.clone();
                let payload_clone = payload.clone();
                writer_handles.push(thread::spawn(move || {
                    let mut writer = Queue::open_publisher(&path).expect("writer open");
                    barrier_clone.wait();
                    for _ in 0..msgs_per_writer_iter {
                        writer.append(1, black_box(&payload_clone)).expect("append");
                    }
                }));
            }

            barrier.wait();
            let start = Instant::now();

            let mut received = 0;
            while received < total_msgs_to_read {
                if let Some(_) = fanin.next().expect("read") {
                    received += 1;
                }
            }

            for h in writer_handles {
                h.join().expect("writer join");
            }

            start.elapsed()
        })
    });
    group.finish();
}

criterion_group!(benches, bench_fanin_throughput);
criterion_main!(benches);
