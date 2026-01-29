//! TimeSeriesLog demonstration: writing and querying time-series data.
//!
//! This example shows how to:
//! 1. Write time-series data with timestamps
//! 2. Seek to specific timestamps
//! 3. Query time ranges efficiently
//! 4. Handle edge cases (before start, after end)
//!
//! Run with: cargo run --example timeseries_demo

use chronicle::core::{SeekResult, TimeSeriesReader, TimeSeriesWriter};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== TimeSeriesLog Demo ===\n");

    let temp_dir = tempfile::tempdir()?;
    let log_path = temp_dir.path();

    // ====================
    // Part 1: Write Data
    // ====================
    println!("1. Writing time-series data...");
    let mut writer = TimeSeriesWriter::open(log_path)?;

    // Simulate market ticks over 1 hour (3600 seconds)
    // One tick per second: timestamps 1000000000, 1000001000, 1000002000, ...
    let base_ts = 1_000_000_000u64; // Starting timestamp (1 second in nanoseconds)
    let tick_count = 3600; // 1 hour of second-resolution data

    for i in 0..tick_count {
        let timestamp_ns = base_ts + (i * 1_000_000_000); // i seconds
        let price = 50000.0 + (i as f64 * 0.1); // Simulated price movement
        let volume = 100 + (i % 50); // Simulated volume

        let data = format!("BTCUSDT,{:.2},{}", price, volume);
        writer.append(0x01, timestamp_ns, data.as_bytes())?;
    }

    writer.finish()?;
    println!("   ✓ Wrote {} ticks", tick_count);
    println!("   ✓ Time range: {}ns - {}ns\n", base_ts, base_ts + 3599_000_000_000);

    // ====================
    // Part 2: Sequential Read
    // ====================
    println!("2. Sequential read (first 5 messages):");
    let mut reader = TimeSeriesReader::open(log_path)?;

    for i in 0..5 {
        if let Some(msg) = reader.next()? {
            let data = std::str::from_utf8(msg.payload)?;
            println!("   [{}] ts={} data={}", i, msg.timestamp_ns, data);
        }
    }
    println!();

    // ====================
    // Part 3: Seek to Timestamp
    // ====================
    println!("3. Seek to specific timestamp:");

    // Seek to 30 minutes in (1800 seconds)
    let target_ts = base_ts + (1800 * 1_000_000_000);
    println!("   Seeking to timestamp: {}ns (30 minutes in)...", target_ts);

    let start = Instant::now();
    let result = reader.seek_timestamp(target_ts)?;
    let seek_duration = start.elapsed();

    match result {
        SeekResult::Found => {
            println!("   ✓ Seek succeeded in {:?}", seek_duration);
            if let Some(msg) = reader.next()? {
                let data = std::str::from_utf8(msg.payload)?;
                println!("   First message: ts={} data={}", msg.timestamp_ns, data);
            }
        }
        _ => println!("   ✗ Seek failed: {:?}", result),
    }
    println!();

    // ====================
    // Part 4: Time Range Query
    // ====================
    println!("4. Time range query:");

    // Query 5-minute window starting at 15 minutes
    let range_start = base_ts + (900 * 1_000_000_000); // 15 minutes
    let range_end = base_ts + (1200 * 1_000_000_000); // 20 minutes

    println!("   Querying range: {}ns - {}ns", range_start, range_end);
    println!("   (15 to 20 minutes, should be ~300 messages)");

    reader.seek_timestamp(range_start)?;

    let mut count = 0;
    let start = Instant::now();
    while let Some(msg) = reader.next()? {
        if msg.timestamp_ns >= range_end {
            break;
        }
        count += 1;
    }
    let scan_duration = start.elapsed();

    println!("   ✓ Found {} messages in {:?}", count, scan_duration);
    println!("   ✓ Throughput: {:.0} msg/sec\n", count as f64 / scan_duration.as_secs_f64());

    // ====================
    // Part 5: Edge Cases
    // ====================
    println!("5. Edge cases:");

    // Seek before start
    let before_start = base_ts - 1_000_000_000;
    println!("   Seeking before start ({}ns)...", before_start);
    let result = reader.seek_timestamp(before_start)?;
    println!("   Result: {:?}", result);

    // Seek after end
    let after_end = base_ts + 10_000_000_000_000; // Way past the end
    println!("   Seeking after end ({}ns)...", after_end);
    let result = reader.seek_timestamp(after_end)?;
    println!("   Result: {:?}", result);

    println!();

    // ====================
    // Part 6: Multiple Range Queries
    // ====================
    println!("6. Multiple range queries (backtesting simulation):");

    let windows = [
        (0, 300),     // First 5 minutes
        (1800, 2100), // 30-35 minutes
        (3300, 3600), // Last 5 minutes
    ];

    for (start_offset, end_offset) in &windows {
        let range_start = base_ts + (start_offset * 1_000_000_000);
        let range_end = base_ts + (end_offset * 1_000_000_000);

        reader.seek_timestamp(range_start)?;

        let mut count = 0;
        let mut sum_price = 0.0;

        while let Some(msg) = reader.next()? {
            if msg.timestamp_ns >= range_end {
                break;
            }

            // Parse price from "BTCUSDT,price,volume"
            let data = std::str::from_utf8(msg.payload)?;
            if let Some(price_str) = data.split(',').nth(1) {
                if let Ok(price) = price_str.parse::<f64>() {
                    sum_price += price;
                    count += 1;
                }
            }
        }

        let avg_price = if count > 0 { sum_price / count as f64 } else { 0.0 };
        println!(
            "   Window {}-{} minutes: {} messages, avg_price={:.2}",
            start_offset / 60,
            end_offset / 60,
            count,
            avg_price
        );
    }

    println!("\n=== Demo Complete ===");
    Ok(())
}
