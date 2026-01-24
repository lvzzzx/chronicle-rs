use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use chronicle::core::reader::WaitStrategy;
use chronicle::core::Queue;
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use tempfile::tempdir;

const MSG_SIZE: usize = 256;
const LATENCY_SAMPLES: usize = 100_000;
const THROUGHPUT_BATCH: usize = 1_000_000;

fn bench_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("ipc_throughput");
    group.measurement_time(Duration::from_secs(40));
    group.throughput(Throughput::Elements(THROUGHPUT_BATCH as u64));

    group.bench_function("1_writer_1_reader", |b| {
        b.iter_custom(|iters| {
            let dir = tempdir().expect("tempdir");
            let path = dir.path().join("throughput_q");
            
            // Setup Writer
            let mut writer = Queue::open_publisher(&path).expect("writer");
            
            // Setup Reader
            let mut reader = Queue::open_subscriber(&path, "bench_reader").expect("reader");
            // Use BusySpin for max throughput measurement
            reader.set_wait_strategy(WaitStrategy::BusySpin);

            let payload = vec![0u8; MSG_SIZE];
            let barrier = Arc::new(Barrier::new(2));
            let barrier_clone = barrier.clone();

            let total_msgs = THROUGHPUT_BATCH as u64 * iters;

            let reader_handle = thread::spawn(move || {
                barrier_clone.wait();
                let mut received = 0;
                while received < total_msgs {
                    if let Some(_) = reader.next().expect("read") {
                        received += 1;
                    }
                }
            });

            barrier.wait();
            let start = Instant::now();
            
            for _ in 0..total_msgs {
                writer.append(1, black_box(&payload)).expect("append");
            }

            reader_handle.join().expect("reader join");
            start.elapsed()
        })
    });
    group.finish();
}

fn run_latency_test(name: &str, strategy: WaitStrategy) {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("latency_q");

    let mut writer = Queue::open_publisher(&path).expect("writer");
    let mut reader = Queue::open_subscriber(&path, "bench_reader").expect("reader");
    reader.set_wait_strategy(strategy);

    // Payload must be large enough to hold u128 or u64 timestamp
    assert!(MSG_SIZE >= 16);
    let mut payload = vec![0u8; MSG_SIZE];
    let mut latencies = Vec::with_capacity(LATENCY_SAMPLES);

    println!("\nRunning IPC Latency Benchmark: {} ({} samples)...", name, LATENCY_SAMPLES);
    
    // Warmup
    for _ in 0..1000 {
        writer.append(1, &payload).unwrap();
        while reader.next().unwrap().is_none() {}
    }

    // Reference start time for monotonic diffs
    let bench_start = Instant::now();

    for _ in 0..LATENCY_SAMPLES {
        // Embed monotonic timestamp in first 16 bytes of payload
        let send_time = bench_start.elapsed().as_nanos();
        payload[0..16].copy_from_slice(&send_time.to_le_bytes());

        // We still provide a dummy wall-clock to the API
        writer.append(1, &payload).unwrap();
        
        loop {
            if let Some(msg) = reader.next().unwrap() {
                let recv_time = bench_start.elapsed().as_nanos();
                
                // Extract send time from payload
                let mut ts_buf = [0u8; 16];
                ts_buf.copy_from_slice(&msg.payload[0..16]);
                let send_ts = u128::from_le_bytes(ts_buf);

                let latency = recv_time.saturating_sub(send_ts);
                latencies.push(latency);
                break;
            }
            // Busy wait for the benchmark driver to get precise timing
        }
        
        // Rate limit: 10us delay between messages to simulate "steady state" rather than "burst"
        let limit_start = Instant::now();
        while limit_start.elapsed().as_micros() < 10 {
            std::hint::spin_loop();
        }
    }

    latencies.sort_unstable();
    let p50 = latencies[latencies.len() / 2];
    let p99 = latencies[(latencies.len() as f64 * 0.99) as usize];
    let p999 = latencies[(latencies.len() as f64 * 0.999) as usize];
    let max = latencies[latencies.len() - 1];

    println!("{} Latency Distribution (ns):", name);
    println!("  Min:  {}", latencies[0]);
    println!("  P50:  {}", p50);
    println!("  P99:  {}", p99);
    println!("  P999: {}", p999);
    println!("  Max:  {}", max);
}

fn bench_latency_suite(_c: &mut Criterion) {
    run_latency_test("BusySpin", WaitStrategy::BusySpin);
    // 10us spin is typical for hybrid strategies
    run_latency_test("SpinThenPark(10us)", WaitStrategy::SpinThenPark { spin_us: 10 });
}

criterion_group!(benches, bench_throughput, bench_latency_suite);
criterion_main!(benches);
