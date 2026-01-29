//! Rolling Writer Example
//!
//! This example demonstrates the RollingWriter API for automatic partition management:
//! - Writing continuously across date boundaries
//! - Hour-based partition rolling
//! - Timezone handling (UTC, Asia/Shanghai)
//! - Rolling statistics
//! - Custom partition callbacks
//!
//! Run with:
//! ```bash
//! cargo run --example table_rolling
//! ```

use chronicle::core::Result;
use chronicle::table::{
    DateRoller, HourRoller, PartitionScheme, PartitionValues, Table, TableConfig, Timezone,
};

fn main() -> Result<()> {
    println!("=== Chronicle Rolling Writer Example ===\n");

    let temp_dir = tempfile::tempdir().unwrap();
    let table_path = temp_dir.path().join("streaming_data");

    example_1_date_rolling(&table_path)?;
    example_2_hour_rolling(&table_path)?;
    example_3_timezone_handling(&table_path)?;
    example_4_rolling_stats(&table_path)?;

    println!("\n✓ All rolling writer examples completed!");

    Ok(())
}

/// Example 1: Date-based rolling (partition per day)
fn example_1_date_rolling(table_path: &std::path::Path) -> Result<()> {
    println!("Example 1: Date-Based Rolling\n");

    // Create table with venue + symbol + date partitions
    let scheme = PartitionScheme::new()
        .add_string("venue")
        .add_string("symbol")
        .add_date("date");

    let table = Table::create(table_path, scheme, TableConfig::default())?;

    // Static partition values (venue and symbol don't change)
    let static_partition = PartitionValues::new()
        .insert("venue", "binance")
        .insert("symbol", "BTC-USDT");

    // Create rolling writer that auto-manages the date partition
    let roller = DateRoller::new(Timezone::Utc);
    let mut writer = table.rolling_writer("date", static_partition, Box::new(roller))?;

    println!("Writing messages across 3 days...");

    // Day 1: 2026-01-29
    let base_ts = 1738108800_000_000_000_u64; // 2026-01-29 00:00:00 UTC
    for i in 0..100 {
        let timestamp_ns = base_ts + (i * 100_000_000); // 100ms apart
        let payload = format!("day1_msg_{}", i);
        writer.append(0x01, timestamp_ns, payload.as_bytes())?;
    }
    println!("  ✓ Wrote 100 messages to date=2026-01-29");

    // Day 2: 2026-01-30 (automatically rolls to new partition)
    let day2_ts = base_ts + 86400_000_000_000; // +24 hours
    for i in 0..100 {
        let timestamp_ns = day2_ts + (i * 100_000_000);
        let payload = format!("day2_msg_{}", i);
        writer.append(0x01, timestamp_ns, payload.as_bytes())?;
    }
    println!("  ✓ Wrote 100 messages to date=2026-01-30");

    // Day 3: 2026-01-31
    let day3_ts = day2_ts + 86400_000_000_000; // +24 hours
    for i in 0..100 {
        let timestamp_ns = day3_ts + (i * 100_000_000);
        let payload = format!("day3_msg_{}", i);
        writer.append(0x01, timestamp_ns, payload.as_bytes())?;
    }
    println!("  ✓ Wrote 100 messages to date=2026-01-31");

    writer.finish()?;

    println!("\n  Created 3 date partitions automatically!");
    println!();

    Ok(())
}

/// Example 2: Hour-based rolling (partition per hour)
fn example_2_hour_rolling(table_path: &std::path::Path) -> Result<()> {
    println!("Example 2: Hour-Based Rolling\n");

    let table = Table::open(table_path)?;

    let static_partition = PartitionValues::new()
        .insert("venue", "coinbase")
        .insert("symbol", "ETH-USD");

    // Create rolling writer with hour-based partitions
    let roller = HourRoller::new(Timezone::Utc);
    let mut writer = table.rolling_writer("date", static_partition, Box::new(roller))?;

    println!("Writing messages across 4 hours...");

    let base_ts = 1738108800_000_000_000_u64; // 2026-01-29 00:00:00 UTC

    // Write to 4 consecutive hours
    for hour in 0..4 {
        let hour_ts = base_ts + (hour * 3600_000_000_000); // +1 hour each iteration
        for i in 0..50 {
            let timestamp_ns = hour_ts + (i * 60_000_000_000); // 1 minute apart
            let payload = format!("hour{}_msg_{}", hour, i);
            writer.append(0x02, timestamp_ns, payload.as_bytes())?;
        }
        println!("  ✓ Wrote 50 messages to hour {}", hour);
    }

    writer.finish()?;

    println!("\n  Created 4 hour partitions automatically!");
    println!();

    Ok(())
}

/// Example 3: Timezone handling
fn example_3_timezone_handling(table_path: &std::path::Path) -> Result<()> {
    println!("Example 3: Timezone Handling\n");

    let table = Table::open(table_path)?;

    // Use Asia/Shanghai timezone (UTC+8)
    let static_partition = PartitionValues::new()
        .insert("venue", "binance")
        .insert("symbol", "BTC-USDT");

    let roller = DateRoller::new(Timezone::AsiaShanghai);
    let mut writer = table.rolling_writer("date", static_partition, Box::new(roller))?;

    println!("Using Asia/Shanghai timezone (UTC+8)");
    println!("Writing messages around midnight Shanghai time...\n");

    // 2026-01-29 16:00:00 UTC = 2026-01-30 00:00:00 Shanghai (midnight)
    let shanghai_midnight_utc = 1738108800_000_000_000_u64 + 16 * 3600_000_000_000;

    // Write before midnight Shanghai
    for i in 0..50 {
        let timestamp_ns = shanghai_midnight_utc - 3600_000_000_000 + (i * 60_000_000_000);
        let payload = format!("before_midnight_{}", i);
        writer.append(0x01, timestamp_ns, payload.as_bytes())?;
    }
    println!("  ✓ Wrote 50 messages before midnight (Shanghai)");

    // Write after midnight Shanghai (new date partition)
    for i in 0..50 {
        let timestamp_ns = shanghai_midnight_utc + (i * 60_000_000_000);
        let payload = format!("after_midnight_{}", i);
        writer.append(0x01, timestamp_ns, payload.as_bytes())?;
    }
    println!("  ✓ Wrote 50 messages after midnight (Shanghai)");

    writer.finish()?;

    println!("\n  Partitions created based on Shanghai timezone!");
    println!();

    Ok(())
}

/// Example 4: Rolling statistics
fn example_4_rolling_stats(table_path: &std::path::Path) -> Result<()> {
    println!("Example 4: Rolling Statistics\n");

    let table = Table::open(table_path)?;

    let static_partition = PartitionValues::new()
        .insert("venue", "kraken")
        .insert("symbol", "BTC-EUR");

    let roller = DateRoller::new(Timezone::Utc);
    let mut writer = table.rolling_writer("date", static_partition, Box::new(roller))?;

    // Write across multiple days
    let base_ts = 1738108800_000_000_000_u64;

    for day in 0..3 {
        let day_ts = base_ts + (day * 86400_000_000_000);
        for i in 0..200 {
            let timestamp_ns = day_ts + (i * 50_000_000);
            let payload = b"market_data";
            writer.append(0x01, timestamp_ns, payload)?;
        }
    }

    // Get statistics before finishing
    let stats = writer.stats();
    println!("Rolling Writer Statistics:");
    println!("  Total messages:   {}", stats.total_messages);
    println!("  Total bytes:      {}", stats.total_bytes);
    println!("  Partitions:       {}", stats.partitions_written);
    println!("  Segments:         {}", stats.segments_written);
    println!("  Rolls:            {}", stats.rolls_performed);

    writer.finish()?;

    println!();

    Ok(())
}
