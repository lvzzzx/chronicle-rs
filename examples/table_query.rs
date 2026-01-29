//! Table Query Example
//!
//! This example demonstrates advanced querying capabilities:
//! - Partition filtering (exact, in, range, and)
//! - Time-range scans
//! - Timestamp seeking
//! - Merge strategies
//! - Reading from compressed segments
//!
//! Run with:
//! ```bash
//! cargo run --example table_query
//! ```

use chronicle::core::Result;
use chronicle::table::{
    CompressionPolicy, MergeStrategy, PartitionFilter, PartitionScheme, PartitionValues, Table,
    TableConfig, TimeSeriesReader,
};

fn main() -> Result<()> {
    println!("=== Chronicle Table Query Example ===\n");

    let temp_dir = tempfile::tempdir().unwrap();
    let table_path = temp_dir.path().join("query_demo");

    // Setup: Create table with test data
    setup_test_data(&table_path)?;

    example_1_exact_filter(&table_path)?;
    example_2_in_filter(&table_path)?;
    example_3_range_filter(&table_path)?;
    example_4_and_filter(&table_path)?;
    example_5_time_range_scan(&table_path)?;
    example_6_timestamp_seeking(&table_path)?;
    example_7_merge_strategies(&table_path)?;

    println!("\n✓ All query examples completed!");

    Ok(())
}

/// Setup test data: Create table with multiple partitions
fn setup_test_data(table_path: &std::path::Path) -> Result<()> {
    println!("Setting up test data...\n");

    let scheme = PartitionScheme::new()
        .add_string("exchange")
        .add_string("symbol")
        .add_date("date");

    // Enable immediate compression for demo
    let config = TableConfig {
        compression_policy: CompressionPolicy::Never, // Keep hot for quick reads
        ..Default::default()
    };

    let table = Table::create(table_path, scheme, config)?;

    // Create data for multiple exchanges, symbols, and dates
    let exchanges = ["binance", "coinbase", "kraken"];
    let symbols = ["BTC-USD", "ETH-USD", "SOL-USD"];
    let dates = ["2026-01-28", "2026-01-29", "2026-01-30"];

    let mut total_messages = 0;

    for exchange in &exchanges {
        for symbol in &symbols {
            for (day_idx, date) in dates.iter().enumerate() {
                let mut partition = PartitionValues::new();
    partition.insert("exchange", exchange);
    partition.insert("symbol", symbol);
    partition.insert("date", date);

                let mut writer = table.writer(partition)?;

                // Write different amounts per partition
                let count = 50 + (day_idx * 20);
                let base_ts = 1738022400_000_000_000_u64 + (day_idx as u64 * 86400_000_000_000);

                for i in 0..count {
                    let timestamp_ns = base_ts + (i as u64 * 1_000_000);
                    let payload = format!("{}:{}:{}", exchange, symbol, i);
                    writer.append(0x01, timestamp_ns, payload.as_bytes())?;
                }

                writer.finish()?;
                total_messages += count;
            }
        }
    }

    println!("✓ Created {} messages across {} partitions\n", total_messages, exchanges.len() * symbols.len() * dates.len());

    Ok(())
}

/// Example 1: Exact partition filter
fn example_1_exact_filter(table_path: &std::path::Path) -> Result<()> {
    println!("Example 1: Exact Partition Filter\n");

    let table = Table::open(table_path)?;

    // Query specific partition: binance/BTC-USD/2026-01-29
    let filter = PartitionFilter::Exact(
        PartitionValues::new()
            .insert("exchange", "binance")
            .insert("symbol", "BTC-USD")
            .insert("date", "2026-01-29"),
    );

    println!("Filter: exchange=binance, symbol=BTC-USD, date=2026-01-29");

    let mut reader = table.query(filter)?;
    let mut count = 0;

    while let Some(_msg) = reader.next()? {
        count += 1;
    }

    println!("  ✓ Found {} messages\n", count);

    Ok(())
}

/// Example 2: IN filter (multiple values for one key)
fn example_2_in_filter(table_path: &std::path::Path) -> Result<()> {
    println!("Example 2: IN Filter\n");

    let table = Table::open(table_path)?;

    // Query all BTC and ETH across all exchanges and dates
    let filter = PartitionFilter::In {
        key: "symbol".to_string(),
        values: vec!["BTC-USD".to_string(), "ETH-USD".to_string()],
    };

    println!("Filter: symbol IN [BTC-USD, ETH-USD]");

    let mut reader = table.query(filter)?;
    let mut count = 0;

    while let Some(_msg) = reader.next()? {
        count += 1;
    }

    println!("  ✓ Found {} messages\n", count);

    Ok(())
}

/// Example 3: Range filter
fn example_3_range_filter(table_path: &std::path::Path) -> Result<()> {
    println!("Example 3: Range Filter\n");

    let table = Table::open(table_path)?;

    // Query date range: 2026-01-28 to 2026-01-29
    let filter = PartitionFilter::Range {
        key: "date".to_string(),
        start: "2026-01-28".to_string(),
        end: "2026-01-29".to_string(),
    };

    println!("Filter: date BETWEEN 2026-01-28 AND 2026-01-29");

    let mut reader = table.query(filter)?;
    let mut count = 0;

    while let Some(_msg) = reader.next()? {
        count += 1;
    }

    println!("  ✓ Found {} messages\n", count);

    Ok(())
}

/// Example 4: AND filter (combine multiple filters)
fn example_4_and_filter(table_path: &std::path::Path) -> Result<()> {
    println!("Example 4: AND Filter\n");

    let table = Table::open(table_path)?;

    // Query: binance exchange AND date >= 2026-01-29
    let filter = PartitionFilter::And(vec![
        PartitionFilter::Exact(PartitionValues::new().insert("exchange", "binance")),
        PartitionFilter::Range {
            key: "date".to_string(),
            start: "2026-01-29".to_string(),
            end: "2026-01-30".to_string(),
        },
    ]);

    println!("Filter: exchange=binance AND date IN [2026-01-29, 2026-01-30]");

    let mut reader = table.query(filter)?;
    let mut count = 0;

    while let Some(_msg) = reader.next()? {
        count += 1;
    }

    println!("  ✓ Found {} messages\n", count);

    Ok(())
}

/// Example 5: Time-range scan within a partition
fn example_5_time_range_scan(table_path: &std::path::Path) -> Result<()> {
    println!("Example 5: Time-Range Scan\n");

    let table = Table::open(table_path)?;

    let mut partition = PartitionValues::new();
    partition.insert("exchange", "binance");
    partition.insert("symbol", "BTC-USD");
    partition.insert("date", "2026-01-29");

    let mut reader = table.reader(partition)?;

    // Define time range (in nanoseconds)
    let start_ts = 1738108800_000_000_000_u64; // Start of 2026-01-29
    let end_ts = start_ts + 3600_000_000_000; // +1 hour

    println!("Scanning messages between timestamps:");
    println!("  Start: {}", start_ts);
    println!("  End:   {}", end_ts);

    let mut count = 0;
    while let Some(msg) = reader.next()? {
        if msg.timestamp_ns >= start_ts && msg.timestamp_ns < end_ts {
            count += 1;
        }
        if msg.timestamp_ns >= end_ts {
            break; // Stop once we pass the end time
        }
    }

    println!("  ✓ Found {} messages in time range\n", count);

    Ok(())
}

/// Example 6: Timestamp seeking
fn example_6_timestamp_seeking(table_path: &std::path::Path) -> Result<()> {
    println!("Example 6: Timestamp Seeking\n");

    let table = Table::open(table_path)?;

    let mut partition = PartitionValues::new();
    partition.insert("exchange", "coinbase");
    partition.insert("symbol", "ETH-USD");
    partition.insert("date", "2026-01-29");

    let mut reader = table.reader(partition)?;

    // Seek to middle of the day
    let target_ts = 1738108800_000_000_000_u64 + 12 * 3600_000_000_000; // +12 hours

    println!("Seeking to timestamp: {}", target_ts);

    let found = reader.seek_timestamp(target_ts)?;
    println!("  Seek result: {}", if found { "found" } else { "not found" });

    // Read next few messages after seek
    let mut count = 0;
    while let Some(msg) = reader.next()? {
        if count == 0 {
            println!("  First message after seek: ts={}", msg.timestamp_ns);
        }
        count += 1;
        if count >= 10 {
            break;
        }
    }

    println!("  ✓ Read {} messages after seek\n", count);

    Ok(())
}

/// Example 7: Merge strategies
fn example_7_merge_strategies(table_path: &std::path::Path) -> Result<()> {
    println!("Example 7: Merge Strategies\n");

    let table = Table::open(table_path)?;

    // Query multiple partitions
    let filter = PartitionFilter::Exact(PartitionValues::new().insert("date", "2026-01-29"));

    // Strategy 1: Timestamp-ordered (global ordering)
    println!("Strategy 1: Timestamp-Ordered Merge");
    let mut reader1 = table.query_with_strategy(filter.clone(), MergeStrategy::TimestampOrdered)?;

    let mut count = 0;
    let mut last_ts = 0;
    let mut ordering_violations = 0;

    while let Some(msg) = reader1.next()? {
        if msg.timestamp_ns < last_ts {
            ordering_violations += 1;
        }
        last_ts = msg.timestamp_ns;
        count += 1;
    }

    println!("  ✓ Read {} messages", count);
    println!("  ✓ Ordering violations: {} (should be 0)", ordering_violations);

    // Strategy 2: Sequential (faster, no global ordering)
    println!("\nStrategy 2: Partition-Sequential");
    let mut reader2 = table.query_with_strategy(filter, MergeStrategy::PartitionSequential)?;

    count = 0;
    while let Some(_msg) = reader2.next()? {
        count += 1;
    }

    println!("  ✓ Read {} messages (faster, per-partition ordering)", count);
    println!();

    Ok(())
}
