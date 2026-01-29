//! Basic Table Usage Example
//!
//! Demonstrates core Table API functionality.

use chronicle::core::Result;
use chronicle::table::{PartitionFilter, PartitionScheme, PartitionValues, Table};

fn main() -> Result<()> {
    println!("=== Chronicle Table Basic Usage ===\n");

    let temp_dir = tempfile::tempdir().unwrap();
    let table_path = temp_dir.path().join("market_data");

    // Example 1: Create a table
    println!("Example 1: Creating a Table\n");

    let scheme = PartitionScheme::new()
        .add_string("venue")
        .add_string("symbol")
        .add_date("date");

    let table = Table::create(&table_path, scheme)?;
    println!("✓ Table created\n");

    // Example 2: Write to partitions
    println!("Example 2: Writing to Partitions\n");

    let mut partition1 = PartitionValues::new();
    partition1.insert("venue", "binance");
    partition1.insert("symbol", "BTC-USDT");
    partition1.insert("date", "2026-01-29");

    let mut writer1 = table.writer(partition1)?;
    for i in 0..100 {
        let timestamp_ns = 1738108800_000_000_000 + (i * 1_000_000);
        let payload = format!("trade_{}", i);
        writer1.append(0x01, timestamp_ns, payload.as_bytes())?;
    }
    writer1.finish()?;
    println!("✓ Wrote 100 messages to binance/BTC-USDT/2026-01-29\n");

    let mut partition2 = PartitionValues::new();
    partition2.insert("venue", "binance");
    partition2.insert("symbol", "ETH-USDT");
    partition2.insert("date", "2026-01-29");

    let mut writer2 = table.writer(partition2)?;
    for i in 0..50 {
        let timestamp_ns = 1738108800_000_000_000 + (i * 2_000_000);
        let payload = format!("trade_{}", i);
        writer2.append(0x02, timestamp_ns, payload.as_bytes())?;
    }
    writer2.finish()?;
    println!("✓ Wrote 50 messages to binance/ETH-USDT/2026-01-29\n");

    // Example 3: Read from a partition
    println!("Example 3: Reading from a Partition\n");

    let mut read_partition = PartitionValues::new();
    read_partition.insert("venue", "binance");
    read_partition.insert("symbol", "BTC-USDT");
    read_partition.insert("date", "2026-01-29");

    let mut reader = table.reader(read_partition)?;
    let mut count = 0;
    while let Some(_msg) = reader.next()? {
        count += 1;
    }
    println!("✓ Read {} messages\n", count);

    // Example 4: List partitions
    println!("Example 4: Listing Partitions\n");

    let partitions = table.partitions()?;
    println!("Found {} partitions:", partitions.len());
    for info in &partitions {
        println!("  - {}", info.values.to_path_string());
    }
    println!();

    // Example 5: Query partitions
    println!("Example 5: Querying Partitions\n");

    let filter = PartitionFilter::All;
    let mut query_reader = table.query(filter)?;
    count = 0;
    while let Some(_msg) = query_reader.next()? {
        count += 1;
    }
    println!("✓ Read {} total messages from all partitions\n", count);

    println!("✓ All examples completed successfully!");

    Ok(())
}
