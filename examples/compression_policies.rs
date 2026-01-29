//! Example: Using compression policies with time-series tables.
//!
//! This example demonstrates:
//! - Creating tables with different compression policies
//! - Writing time-series data
//! - Running lifecycle management to compress old data
//! - Reading from compressed segments transparently

use chronicle::core::Result;
use chronicle::lifecycle::{LifecycleConfig, StorageLifecycleManager};
use chronicle::table::{
    CompressionPolicy, PartitionScheme, PartitionValues, Table, TableConfig,
};

fn main() -> Result<()> {
    println!("=== Compression Policies Example ===\n");

    // Example 1: Immediate compression (archival use case)
    example_immediate_compression()?;

    // Example 2: Age-based compression (time-series use case)
    example_age_based_compression()?;

    // Example 3: Idle-based compression (IPC use case)
    example_idle_based_compression()?;

    // Example 4: Manual lifecycle management
    example_manual_lifecycle()?;

    Ok(())
}

/// Example 1: Compress immediately after seal (archival).
fn example_immediate_compression() -> Result<()> {
    println!("Example 1: Immediate Compression (Archival)\n");

    let temp_dir = tempfile::tempdir().unwrap();
    let table_path = temp_dir.path().join("archival_data");

    // Create table with immediate compression
    let scheme = PartitionScheme::new()
        .add_string("dataset")
        .add_date("date");

    let config = TableConfig {
        compression_policy: CompressionPolicy::Immediate,
        compression_level: 6, // Higher compression for archival
        ..Default::default()
    };

    let table = Table::create(&table_path, scheme, config)?;

    // Write data
    let partition = PartitionValues::new()
        .set("dataset", "historical_trades")
        .set("date", "2026-01-29");

    let mut writer = table.writer(partition)?;

    for i in 0..1000 {
        let timestamp = 1706486400000000000 + (i * 1000000); // 1ms apart
        let payload = format!("trade_data_{}", i);
        writer.append(0x01, timestamp, payload.as_bytes())?;
    }

    writer.finish()?;

    println!("✓ Wrote 1000 messages to archival table");
    println!("✓ Policy: Compress immediately after seal");
    println!();

    Ok(())
}

/// Example 2: Compress after age (time-series analytics).
fn example_age_based_compression() -> Result<()> {
    println!("Example 2: Age-Based Compression (Time-Series)\n");

    let temp_dir = tempfile::tempdir().unwrap();
    let table_path = temp_dir.path().join("timeseries_data");

    // Create table with age-based compression
    let scheme = PartitionScheme::new()
        .add_string("symbol")
        .add_date("date");

    let config = TableConfig {
        compression_policy: CompressionPolicy::AgeAfter {
            days: 1, // Compress yesterday's data
        },
        compression_level: 3,
        ..Default::default()
    };

    let table = Table::create(&table_path, scheme, config)?;

    // Write today's data
    let partition = PartitionValues::new()
        .set("symbol", "AAPL")
        .set("date", "2026-01-29");

    let mut writer = table.writer(partition)?;

    for i in 0..500 {
        let timestamp = 1706486400000000000 + (i * 1000000);
        let payload = format!("price={}", 150.0 + (i as f64 * 0.01));
        writer.append(0x01, timestamp, payload.as_bytes())?;
    }

    writer.finish()?;

    println!("✓ Wrote 500 messages to time-series table");
    println!("✓ Policy: Compress data older than 1 day");
    println!("✓ Today's data stays hot, yesterday's gets compressed");
    println!();

    Ok(())
}

/// Example 3: Compress after idle period (IPC use case).
fn example_idle_based_compression() -> Result<()> {
    println!("Example 3: Idle-Based Compression (IPC)\n");

    let temp_dir = tempfile::tempdir().unwrap();
    let table_path = temp_dir.path().join("ipc_data");

    // Create table with idle-based compression
    let scheme = PartitionScheme::new()
        .add_string("channel")
        .add_date("date");

    let config = TableConfig {
        compression_policy: CompressionPolicy::IdleAfter {
            days: 7,
            track_reads: true, // Track access times
        },
        compression_level: 3,
        track_access: true, // Enable access tracking
        ..Default::default()
    };

    let table = Table::create(&table_path, scheme, config)?;

    // Write to multiple channels
    for channel_id in 1..=3 {
        let partition = PartitionValues::new()
            .set("channel", &channel_id.to_string())
            .set("date", "2026-01-29");

        let mut writer = table.writer(partition)?;

        for i in 0..100 {
            let timestamp = 1706486400000000000 + (i * 1000000);
            let payload = format!("channel_{}_msg_{}", channel_id, i);
            writer.append(0x01, timestamp, payload.as_bytes())?;
        }

        writer.finish()?;
    }

    println!("✓ Wrote 300 messages across 3 channels");
    println!("✓ Policy: Compress after 7 days of no reads");
    println!("✓ Frequently accessed channels stay hot");
    println!();

    Ok(())
}

/// Example 4: Manual lifecycle management.
fn example_manual_lifecycle() -> Result<()> {
    println!("Example 4: Manual Lifecycle Management\n");

    let temp_dir = tempfile::tempdir().unwrap();
    let table_path = temp_dir.path().join("manual_data");

    // Create table
    let scheme = PartitionScheme::new().add_date("date");

    let table = Table::create(&table_path, scheme, TableConfig::default())?;

    // Write data
    let partition = PartitionValues::new().set("date", "2026-01-29");
    let mut writer = table.writer(partition)?;

    for i in 0..200 {
        let timestamp = 1706486400000000000 + (i * 1000000);
        writer.append(0x01, timestamp, &[1, 2, 3, 4])?;
    }

    writer.finish()?;

    // Create lifecycle manager with custom policy
    let lifecycle_config = LifecycleConfig {
        policy: CompressionPolicy::SizeThreshold {
            bytes: 1024, // Compress segments > 1KB
        },
        compression_level: 3,
        block_size: 64 * 1024,
        parallel_workers: 1,
    };

    let mut manager = StorageLifecycleManager::new(&table_path, lifecycle_config)?;

    // Run lifecycle management
    println!("Running lifecycle management...");
    let stats = manager.run_once()?;

    println!("✓ Scanned: {} segments", stats.scanned_count);
    println!("✓ Compressed: {} segments", stats.compressed_count);
    println!("✓ Bytes saved: {} bytes", stats.bytes_saved);
    println!("✓ Duration: {:?}", stats.duration);

    if stats.has_errors() {
        println!("⚠ Errors: {:?}", stats.errors);
    }

    println!();

    Ok(())
}
