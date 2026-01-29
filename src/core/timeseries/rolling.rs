//! Rolling writer implementation.
//!
//! Provides automatic partition switching based on message content.

use crate::core::timeseries::{
    PartitionValues, Table, TimeSeriesWriter,
};
use crate::core::timeseries::rollers::PartitionRoller;
use crate::core::Result;

/// Statistics for a rolling write session.
#[derive(Debug, Clone, Default)]
pub struct RollingStats {
    /// Total messages written.
    pub messages_written: u64,
    /// Number of partition rolls.
    pub partition_rolls: u64,
    /// Number of partitions written to.
    pub partitions_written: usize,
}

/// Rolling writer that automatically switches partitions based on message content.
///
/// The partition roller determines which partition to write to based on
/// timestamp and payload. When the partition changes, the writer automatically
/// closes the current partition and opens a new one.
///
/// # Example
///
/// ```no_run
/// use chronicle::core::timeseries::{Table, PartitionScheme};
///
/// let scheme = PartitionScheme::new()
///     .add_string("channel")
///     .add_date("date");
/// let table = Table::create("./data", scheme)?;
///
/// let mut writer = table.rolling_writer_by_date("date",
///     [("channel", "101")].into())?;
///
/// // Automatically switches partition when date changes
/// for msg in messages {
///     writer.append(0x01, msg.timestamp_ns, &msg.data)?;
/// }
///
/// let stats = writer.finish()?;
/// println!("Wrote {} partitions", stats.partitions_written);
/// # Ok::<(), chronicle::core::Error>(())
/// ```
pub struct RollingWriter {
    table: Table,
    roller: Box<dyn PartitionRoller>,
    current_partition: Option<PartitionValues>,
    current_writer: Option<TimeSeriesWriter>,
    stats: RollingStats,
    on_roll_callback: Option<Box<dyn FnMut(&PartitionValues, &PartitionValues)>>,
}

impl RollingWriter {
    /// Create a new rolling writer.
    ///
    /// Users should typically use `Table::rolling_writer()` instead.
    pub(crate) fn new(table: Table, roller: Box<dyn PartitionRoller>) -> Result<Self> {
        Ok(Self {
            table,
            roller,
            current_partition: None,
            current_writer: None,
            stats: RollingStats::default(),
            on_roll_callback: None,
        })
    }

    /// Set a callback to be invoked on partition rolls.
    ///
    /// The callback receives (old_partition, new_partition).
    pub fn on_partition_roll<F>(mut self, callback: F) -> Self
    where
        F: FnMut(&PartitionValues, &PartitionValues) + 'static,
    {
        self.on_roll_callback = Some(Box::new(callback));
        self
    }

    /// Append a record, automatically rolling partition if needed.
    ///
    /// # Errors
    ///
    /// - `Error::Io`: Failed to write or create partition
    /// - `Error::PayloadTooLarge`: Payload exceeds maximum size
    pub fn append(&mut self, type_id: u16, timestamp_ns: u64, payload: &[u8]) -> Result<()> {
        // Determine target partition
        let target_partition = self.roller.partition_for(timestamp_ns, payload)?;

        // Check if we need to roll
        let needs_roll = match &self.current_partition {
            None => true,
            Some(current) => current != &target_partition,
        };

        if needs_roll {
            self.roll_to_partition(target_partition)?;
        }

        // Write to current partition
        if let Some(writer) = &mut self.current_writer {
            writer.append(type_id, timestamp_ns, payload)?;
            self.stats.messages_written += 1;
        }

        Ok(())
    }

    /// Roll to a new partition.
    fn roll_to_partition(&mut self, new_partition: PartitionValues) -> Result<()> {
        // Finish current writer if exists
        if let Some(mut writer) = self.current_writer.take() {
            writer.finish()?;
        }

        // Invoke callback if set
        if let Some(callback) = &mut self.on_roll_callback {
            if let Some(old_partition) = &self.current_partition {
                callback(old_partition, &new_partition);
            }
        }

        // Create new TimeSeriesWriter
        let ts_writer = self.table.create_ts_writer(new_partition.clone())?;

        // Track first partition as partition 0, subsequent as partition_rolls
        if self.current_partition.is_some() {
            self.stats.partition_rolls += 1;
        }

        self.current_writer = Some(ts_writer);
        self.current_partition = Some(new_partition);

        Ok(())
    }

    /// Flush pending writes to disk.
    pub fn flush(&mut self) -> Result<()> {
        if let Some(writer) = &mut self.current_writer {
            writer.flush()?;
        }
        Ok(())
    }

    /// Finish writing and return statistics.
    pub fn finish(mut self) -> Result<RollingStats> {
        // Finish current writer
        if let Some(mut writer) = self.current_writer.take() {
            writer.finish()?;
        }

        // Calculate partitions written
        self.stats.partitions_written = if self.current_partition.is_some() {
            (self.stats.partition_rolls + 1) as usize
        } else {
            0
        };

        Ok(self.stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::timeseries::{DateRoller, PartitionScheme, Timezone};
    use tempfile::TempDir;

    #[test]
    fn test_rolling_writer_basic() {
        let dir = TempDir::new().unwrap();

        let scheme = PartitionScheme::new()
            .add_string("channel")
            .add_date("date");

        let table = Table::create(dir.path(), scheme.clone()).unwrap();

        // Create rolling writer
        let roller = DateRoller::new(
            scheme,
            "date",
            Timezone::UTC,
            [("channel", "101")].into(),
        );

        let mut writer = RollingWriter::new(table, Box::new(roller)).unwrap();

        // Write messages on same day (no roll)
        let base_ts = 1_706_486_400_000_000_000u64; // 2024-01-29 00:00:00 UTC
        writer.append(0x01, base_ts, b"msg1").unwrap();
        writer.append(0x01, base_ts + 3600_000_000_000, b"msg2").unwrap();

        let stats = writer.finish().unwrap();
        assert_eq!(stats.messages_written, 2);
        assert_eq!(stats.partition_rolls, 0);
        assert_eq!(stats.partitions_written, 1);
    }

    #[test]
    fn test_rolling_writer_date_change() {
        let dir = TempDir::new().unwrap();

        let scheme = PartitionScheme::new()
            .add_string("channel")
            .add_date("date");

        let table = Table::create(dir.path(), scheme.clone()).unwrap();

        let roller = DateRoller::new(
            scheme,
            "date",
            Timezone::UTC,
            [("channel", "101")].into(),
        );

        let mut writer = RollingWriter::new(table, Box::new(roller)).unwrap();

        // Day 1: 2024-01-29
        let day1_ts = 1_706_486_400_000_000_000u64;
        writer.append(0x01, day1_ts, b"day1_msg1").unwrap();
        writer.append(0x01, day1_ts + 3600_000_000_000, b"day1_msg2").unwrap();

        // Day 2: 2024-01-30
        let day2_ts = day1_ts + 86400_000_000_000;
        writer.append(0x01, day2_ts, b"day2_msg1").unwrap();

        let stats = writer.finish().unwrap();
        assert_eq!(stats.messages_written, 3);
        assert_eq!(stats.partition_rolls, 1); // One roll from day1 to day2
        assert_eq!(stats.partitions_written, 2);
    }

    #[test]
    fn test_rolling_writer_callback() {
        let dir = TempDir::new().unwrap();

        let scheme = PartitionScheme::new()
            .add_string("channel")
            .add_date("date");

        let table = Table::create(dir.path(), scheme.clone()).unwrap();

        let roller = DateRoller::new(
            scheme,
            "date",
            Timezone::UTC,
            [("channel", "101")].into(),
        );

        let mut writer = RollingWriter::new(table, Box::new(roller))
            .unwrap()
            .on_partition_roll(move |_old, _new| {
                // Callback invoked on partition roll
            });

        // Write across two days
        let day1_ts = 1_706_486_400_000_000_000u64;
        writer.append(0x01, day1_ts, b"day1").unwrap();

        let day2_ts = day1_ts + 86400_000_000_000;
        writer.append(0x01, day2_ts, b"day2").unwrap();

        writer.finish().unwrap();
    }
}
