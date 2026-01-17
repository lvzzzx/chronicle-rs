use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use tempfile::tempdir;

const MSG_SIZE: usize = 256;
const THROUGHPUT_BATCH: usize = 1_000_000;
const LATENCY_SAMPLES: usize = 100_000;

fn bench_uds_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("uds_throughput");
    group.throughput(Throughput::Elements(THROUGHPUT_BATCH as u64));

    group.bench_function("1_writer_1_reader", |b| {
        b.iter_custom(|iters| {
            let dir = tempdir().expect("tempdir");
            let path = dir.path().join("bench.sock");

            let listener = UnixListener::bind(&path).expect("bind");
            let barrier = Arc::new(Barrier::new(2));
            let barrier_clone = barrier.clone();

            let total_msgs = THROUGHPUT_BATCH as u64 * iters;
            let payload = vec![0u8; MSG_SIZE];

            // Server (Reader)
            let reader_handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                // Set non-blocking to false (blocking is standard for sockets)
                // or we could use read_exact.
                barrier_clone.wait();

                let mut buf = vec![0u8; MSG_SIZE];
                let mut received = 0;
                while received < total_msgs {
                    stream.read_exact(&mut buf).expect("read");
                    received += 1;
                }
            });

            // Client (Writer)
            let mut stream = UnixStream::connect(&path).expect("connect");
            barrier.wait();
            let start = Instant::now();

            for _ in 0..total_msgs {
                stream.write_all(black_box(&payload)).expect("write");
            }

            reader_handle.join().expect("reader join");
            start.elapsed()
        })
    });
    group.finish();
}

fn bench_uds_latency(_c: &mut Criterion) {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("latency.sock");

    let listener = UnixListener::bind(&path).expect("bind");
    let mut client = UnixStream::connect(&path).expect("connect");
    let (mut server, _) = listener.accept().expect("accept");

    // Disable Nagle's algo? Not applicable for UDS, but good practice in TCP.
    // Set buffer sizes? Default is usually good enough for baseline.

    let mut payload = vec![0u8; MSG_SIZE];
    let mut latencies = Vec::with_capacity(LATENCY_SAMPLES);

    println!("\nRunning UDS Latency Benchmark ({} samples)...", LATENCY_SAMPLES);

    let bench_start = Instant::now();

    for _ in 0..LATENCY_SAMPLES {
        let send_time = bench_start.elapsed().as_nanos();
        payload[0..16].copy_from_slice(&send_time.to_le_bytes());

        client.write_all(&payload).unwrap();
        
        // Read exactly MSG_SIZE
        let mut recv_buf = vec![0u8; MSG_SIZE];
        server.read_exact(&mut recv_buf).unwrap();

        let recv_time = bench_start.elapsed().as_nanos();
        
        let mut ts_buf = [0u8; 16];
        ts_buf.copy_from_slice(&recv_buf[0..16]);
        let send_ts = u128::from_le_bytes(ts_buf);

        let latency = recv_time.saturating_sub(send_ts);
        latencies.push(latency);
    }

    latencies.sort_unstable();
    let p50 = latencies[latencies.len() / 2];
    let p99 = latencies[(latencies.len() as f64 * 0.99) as usize];
    let p999 = latencies[(latencies.len() as f64 * 0.999) as usize];
    let max = latencies[latencies.len() - 1];

    println!("UDS Latency Distribution (ns):");
    println!("  Min:  {}", latencies[0]);
    println!("  P50:  {}", p50);
    println!("  P99:  {}", p99);
    println!("  P999: {}", p999);
    println!("  Max:  {}", max);
}

criterion_group!(benches, bench_uds_throughput, bench_uds_latency);
criterion_main!(benches);
