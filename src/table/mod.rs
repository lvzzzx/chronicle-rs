//! Time-series data management.
//!
//! This module provides two layers of abstraction:
//! 1. **TimeSeriesLog**: Single append-only log with timestamp indexing
//! 2. **TimeSeriesTable**: Multi-partition table with automatic rolling
//!
//! # TimeSeriesLog
//!
//! Basic building block for time-series data with timestamp-based seeking.
//!
//! ```no_run
//! use chronicle::core::{TimeSeriesWriter, TimeSeriesReader};
//!
//! // Write time-series data
//! let mut writer = TimeSeriesWriter::open("./market_data")?;
//! writer.append(0x01, timestamp_ns, &data)?;
//! writer.finish()?;
//!
//! // Query time range
//! let mut reader = TimeSeriesReader::open("./market_data")?;
//! reader.seek_timestamp(start_ts)?;
//! while let Some(msg) = reader.next()? {
//!     if msg.timestamp_ns >= end_ts { break; }
//!     process(msg);
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```
//!
//! # TimeSeriesTable
//!
//! Partitioned table with automatic rolling based on message content.
//!
//! ```no_run
//! use chronicle::core::timeseries::{Table, PartitionScheme};
//!
//! // Create table with date partitioning
//! let scheme = PartitionScheme::new()
//!     .add_string("channel")
//!     .add_date("date");
//! let table = Table::create("./l3_data", scheme)?;
//!
//! // Rolling writer automatically switches partitions by date
//! let mut writer = table.rolling_writer_by_date("date",
//!     [("channel", "101")].into())?;
//!
//! for msg in messages {
//!     writer.append(0x01, msg.timestamp_ns, &msg.data)?;
//! }
//! writer.finish()?;
//! # Ok::<(), chronicle::core::Error>(())
//! ```

mod config;
mod log;
mod metadata;
mod partition;
mod rolling;
mod rollers;
mod table;
mod table_reader;

// Re-export config types
pub use config::{CompressionPolicy, TableConfig};

// Re-export log types
pub use log::{SeekResult, TimeSeriesReader, TimeSeriesWriter};

// Re-export table types
pub use metadata::TableMetadata;
pub use partition::{PartitionKey, PartitionScheme, PartitionValue, PartitionValues};
pub use rolling::{RollingStats, RollingWriter};
pub use rollers::{DateRoller, HourRoller, MinuteRoller, PartitionRoller, Timezone};
pub use table::{PartitionInfo, PartitionWriter, Table};
pub use table_reader::{MergeStrategy, PartitionFilter, TableReader};
