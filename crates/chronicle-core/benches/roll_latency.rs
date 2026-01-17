use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use chronicle_core::reader::WaitStrategy;
use chronicle_core::writer::{QueueWriter, WriterConfig};
use chronicle_core::Queue;
use criterion::{criterion_group, criterion_main, Criterion};
use tempfile::tempdir;

const MSG_SIZE: usize = 256;
// 4MB segments to force frequent rolls
const SEGMENT_SIZE: u64 = 4 * 1024 * 1024;
// Send 16MB total (4 rolls)
const TOTAL_MSGS: usize = (16 * 1024 * 1024) / MSG_SIZE;

fn bench_roll_latency(_c: &mut Criterion) {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("roll_latency_q");

    let config = WriterConfig {
        segment_size_bytes: SEGMENT_SIZE,
        ..Default::default()
    };

    let mut writer = Queue::open_publisher_with_config(&path, config).expect("writer");
    let mut reader = Queue::open_subscriber(&path, "bench_reader").expect("reader");
    
    // Use hybrid strategy for realistic measurement
    reader.set_wait_strategy(WaitStrategy::SpinThenPark { spin_us: 10 });

    let mut payload = vec![0u8; MSG_SIZE];
    let mut latencies = Vec::with_capacity(TOTAL_MSGS);

    println!("\nRunning Roll Latency Benchmark...");
    println!("  Segment Size: {} KB", SEGMENT_SIZE / 1024);
    println!("  Total Msgs:   {}", TOTAL_MSGS);
    println!("  Expected Rolls: ~4");

    // Warmup
    for _ in 0..1000 {
        writer.append(1, &payload).unwrap();
        while reader.next().unwrap().is_none() {}
    }

    let bench_start = Instant::now();

    for i in 0..TOTAL_MSGS {
        let send_time = bench_start.elapsed().as_nanos();
        payload[0..16].copy_from_slice(&send_time.to_le_bytes());

        // We want to detect if this specific append triggers a roll
        let old_seg = writer.segment_id();
        writer.append(1, &payload).unwrap();
        let new_seg = writer.segment_id();
        let rolled = new_seg > old_seg;

        loop {
            if let Some(msg) = reader.next().unwrap() {
                let recv_time = bench_start.elapsed().as_nanos();
                
                let mut ts_buf = [0u8; 16];
                ts_buf.copy_from_slice(&msg.payload[0..16]);
                let send_ts = u128::from_le_bytes(ts_buf);

                let latency = recv_time.saturating_sub(send_ts);
                
                if rolled {
                    // Tag roll latencies specifically
                    latencies.push((latency, true));
                } else {
                    latencies.push((latency, false));
                }
                break;
            }
        }

        // Small rate limit to allow sidecar prealloc worker to keep up
        if i % 100 == 0 {
            std::thread::sleep(Duration::from_micros(10));
        }
    }

    let mut steady_latencies: Vec<u128> = latencies.iter().filter(|&&(_, r)| !r).map(|&(l, _)| l).collect();
    let mut roll_latencies: Vec<u128> = latencies.iter().filter(|&&(_, r)| r).map(|&(l, _)| l).collect();

    steady_latencies.sort_unstable();
    roll_latencies.sort_unstable();

    println!("\nSteady State Latency (ns):");
    println!("  P50:  {}", steady_latencies[steady_latencies.len() / 2]);
    println!("  P99:  {}", steady_latencies[(steady_latencies.len() as f64 * 0.99) as usize]);

    println!("\nRoll Event Latency (ns) [{} events]:", roll_latencies.len());
    if !roll_latencies.is_empty() {
        println!("  Min:  {}", roll_latencies[0]);
        println!("  P50:  {}", roll_latencies[roll_latencies.len() / 2]);
        println!("  Max:  {}", roll_latencies[roll_latencies.len() - 1]);
    } else {
        println!("  No roll events captured!");
    }
}

criterion_group!(benches, bench_roll_latency);
criterion_main!(benches);
