//! Multi-partition table reader.
//!
//! Provides querying across multiple partitions with timestamp-ordered merge.

use crate::table::{PartitionInfo, PartitionValues, Table, TimeSeriesReader};
use crate::core::{MessageView, Result};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Merge strategy for reading multiple partitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStrategy {
    /// Merge messages in strict timestamp order across all partitions.
    ///
    /// Uses a min-heap to maintain global timestamp ordering.
    /// Higher overhead but guarantees timestamp order.
    TimestampOrdered,

    /// Read partitions sequentially without global ordering.
    ///
    /// Faster but only maintains per-partition timestamp order.
    PartitionSequential,
}

/// Partition filter for querying.
#[derive(Debug, Clone)]
pub enum PartitionFilter {
    /// Match all partitions.
    All,

    /// Match specific partition values exactly.
    Exact(PartitionValues),

    /// Match partition where key is in a set of values.
    In {
        key: String,
        values: Vec<String>,
    },

    /// Match partition where key is in a range (inclusive).
    Range {
        key: String,
        start: String,
        end: String,
    },

    /// Combine multiple filters with AND logic.
    And(Vec<PartitionFilter>),
}

impl PartitionFilter {
    /// Check if partition matches this filter.
    pub fn matches(&self, partition: &PartitionValues) -> bool {
        match self {
            PartitionFilter::All => true,
            PartitionFilter::Exact(values) => partition == values,
            PartitionFilter::In { key, values } => {
                if let Some(val) = partition.get(key) {
                    values.iter().any(|v| v == &val.to_string())
                } else {
                    false
                }
            }
            PartitionFilter::Range { key, start, end } => {
                if let Some(val) = partition.get(key) {
                    let val_str = val.to_string();
                    val_str >= *start && val_str <= *end
                } else {
                    false
                }
            }
            PartitionFilter::And(filters) => filters.iter().all(|f| f.matches(partition)),
        }
    }
}

/// Multi-partition table reader.
///
/// Reads from multiple partitions with optional timestamp-ordered merging.
///
/// # Example
///
/// ```no_run
/// use chronicle::core::timeseries::{Table, PartitionFilter};
///
/// let table = Table::open("./data")?;
///
/// // Query partitions by date range
/// let filter = PartitionFilter::Range {
///     key: "date".to_string(),
///     start: "2026-01-01".to_string(),
///     end: "2026-01-31".to_string(),
/// };
///
/// let mut reader = table.query(filter)?;
///
/// while let Some(msg) = reader.next()? {
///     println!("ts={} payload_len={}", msg.timestamp_ns, msg.payload.len());
/// }
/// # Ok::<(), chronicle::core::Error>(())
/// ```
pub struct TableReader {
    readers: Vec<PartitionReader>,
    strategy: MergeStrategy,
    heap: Option<BinaryHeap<Reverse<HeapEntry>>>,
    current_partition: usize,
    // Message buffer for returning MessageView
    msg_buffer: Option<(u64, u64, u16, Vec<u8>)>, // (seq, timestamp_ns, type_id, payload)
}

/// Entry in the merge heap.
#[derive(Debug, Eq, PartialEq)]
struct HeapEntry {
    timestamp_ns: u64,
    partition_idx: usize,
    seq: u64,
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Primary: timestamp_ns (ascending)
        // Secondary: partition_idx (for stability)
        // Tertiary: seq (ascending)
        self.timestamp_ns
            .cmp(&other.timestamp_ns)
            .then_with(|| self.partition_idx.cmp(&other.partition_idx))
            .then_with(|| self.seq.cmp(&other.seq))
    }
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Reader for a single partition with buffered message.
struct PartitionReader {
    reader: TimeSeriesReader,
    info: PartitionInfo,
    buffered: Option<(u64, u64, u16, Vec<u8>)>, // (seq, timestamp_ns, type_id, payload)
}

impl TableReader {
    /// Create a new table reader.
    ///
    /// Users should use `Table::query()` instead.
    pub(crate) fn new(
        table: &Table,
        filter: PartitionFilter,
        strategy: MergeStrategy,
    ) -> Result<Self> {
        // Discover partitions matching filter
        let all_partitions = table.partitions()?;
        let filtered: Vec<PartitionInfo> = all_partitions
            .into_iter()
            .filter(|p| filter.matches(&p.values))
            .collect();

        // Open readers for each partition
        let mut readers = Vec::new();
        for info in filtered {
            let reader = TimeSeriesReader::open(&info.path)?;
            readers.push(PartitionReader {
                reader,
                info,
                buffered: None,
            });
        }

        let mut table_reader = Self {
            readers,
            strategy,
            heap: None,
            current_partition: 0,
            msg_buffer: None,
        };

        // Initialize heap for timestamp-ordered strategy
        if strategy == MergeStrategy::TimestampOrdered {
            table_reader.init_heap()?;
        }

        Ok(table_reader)
    }

    /// Initialize the merge heap by reading first message from each partition.
    fn init_heap(&mut self) -> Result<()> {
        let mut heap = BinaryHeap::new();

        for (idx, partition_reader) in self.readers.iter_mut().enumerate() {
            if let Some(msg) = partition_reader.reader.next()? {
                // Buffer the message
                partition_reader.buffered = Some((
                    msg.seq,
                    msg.timestamp_ns,
                    msg.type_id,
                    msg.payload.to_vec(),
                ));

                // Add to heap
                heap.push(Reverse(HeapEntry {
                    timestamp_ns: msg.timestamp_ns,
                    partition_idx: idx,
                    seq: msg.seq,
                }));
            }
        }

        self.heap = Some(heap);
        Ok(())
    }

    /// Read next message.
    pub fn next(&mut self) -> Result<Option<MessageView<'_>>> {
        match self.strategy {
            MergeStrategy::TimestampOrdered => self.next_ordered(),
            MergeStrategy::PartitionSequential => self.next_sequential(),
        }
    }

    /// Read next message in timestamp order.
    fn next_ordered(&mut self) -> Result<Option<MessageView<'_>>> {
        let heap = self.heap.as_mut().unwrap();

        // Pop minimum timestamp entry
        let Some(Reverse(entry)) = heap.pop() else {
            return Ok(None);
        };

        let partition_idx = entry.partition_idx;
        let partition_reader = &mut self.readers[partition_idx];

        // Get buffered message and store in msg_buffer
        let (seq, timestamp_ns, type_id, payload) = partition_reader.buffered.take().unwrap();
        self.msg_buffer = Some((seq, timestamp_ns, type_id, payload));

        // Read next message from this partition and add to heap
        if let Some(next_msg) = partition_reader.reader.next()? {
            partition_reader.buffered = Some((
                next_msg.seq,
                next_msg.timestamp_ns,
                next_msg.type_id,
                next_msg.payload.to_vec(),
            ));

            heap.push(Reverse(HeapEntry {
                timestamp_ns: next_msg.timestamp_ns,
                partition_idx,
                seq: next_msg.seq,
            }));
        }

        // Return view into msg_buffer
        let (seq, timestamp_ns, type_id, payload) = self.msg_buffer.as_ref().unwrap();

        Ok(Some(MessageView {
            seq: *seq,
            timestamp_ns: *timestamp_ns,
            type_id: *type_id,
            payload,
        }))
    }

    /// Read next message sequentially (partition by partition).
    fn next_sequential(&mut self) -> Result<Option<MessageView<'_>>> {
        loop {
            if self.current_partition >= self.readers.len() {
                return Ok(None);
            }

            if let Some(msg) = self.readers[self.current_partition].reader.next()? {
                // Store in msg_buffer and return view
                self.msg_buffer = Some((
                    msg.seq,
                    msg.timestamp_ns,
                    msg.type_id,
                    msg.payload.to_vec(),
                ));

                let (seq, timestamp_ns, type_id, payload) = self.msg_buffer.as_ref().unwrap();

                return Ok(Some(MessageView {
                    seq: *seq,
                    timestamp_ns: *timestamp_ns,
                    type_id: *type_id,
                    payload,
                }));
            } else {
                // Move to next partition
                self.current_partition += 1;
            }
        }
    }

    /// Get number of partitions being read.
    pub fn partition_count(&self) -> usize {
        self.readers.len()
    }

    /// Get partition information.
    pub fn partitions(&self) -> Vec<&PartitionInfo> {
        self.readers.iter().map(|r| &r.info).collect()
    }
}

impl Table {
    /// Query partitions with a filter.
    ///
    /// Returns a reader that merges results in timestamp order.
    pub fn query(&self, filter: PartitionFilter) -> Result<TableReader> {
        TableReader::new(self, filter, MergeStrategy::TimestampOrdered)
    }

    /// Query partitions with custom merge strategy.
    pub fn query_with_strategy(
        &self,
        filter: PartitionFilter,
        strategy: MergeStrategy,
    ) -> Result<TableReader> {
        TableReader::new(self, filter, strategy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::PartitionScheme;
    use tempfile::TempDir;

    #[test]
    fn test_partition_filter_exact() {
        let values: PartitionValues = [("channel", "101"), ("date", "2026-01-29")].into();

        let filter = PartitionFilter::Exact(values.clone());
        assert!(filter.matches(&values));

        let other: PartitionValues = [("channel", "102"), ("date", "2026-01-29")].into();
        assert!(!filter.matches(&other));
    }

    #[test]
    fn test_partition_filter_in() {
        let filter = PartitionFilter::In {
            key: "channel".to_string(),
            values: vec!["101".to_string(), "102".to_string()],
        };

        let values1: PartitionValues = [("channel", "101"), ("date", "2026-01-29")].into();
        assert!(filter.matches(&values1));

        let values2: PartitionValues = [("channel", "103"), ("date", "2026-01-29")].into();
        assert!(!filter.matches(&values2));
    }

    #[test]
    fn test_partition_filter_range() {
        let filter = PartitionFilter::Range {
            key: "date".to_string(),
            start: "2026-01-01".to_string(),
            end: "2026-01-31".to_string(),
        };

        let in_range: PartitionValues = [("date", "2026-01-15")].into();
        assert!(filter.matches(&in_range));

        let out_of_range: PartitionValues = [("date", "2026-02-01")].into();
        assert!(!filter.matches(&out_of_range));
    }

    #[test]
    fn test_table_query_multi_partition() {
        let dir = TempDir::new().unwrap();

        let scheme = PartitionScheme::new()
            .add_string("channel")
            .add_date("date");

        let table = Table::create(dir.path(), scheme).unwrap();

        // Write to multiple partitions
        let p1 = [("channel", "101"), ("date", "2026-01-29")].into();
        let mut w1 = table.writer(p1).unwrap();
        w1.append(0x01, 1000, b"p1_msg1").unwrap();
        w1.append(0x01, 2000, b"p1_msg2").unwrap();
        w1.finish().unwrap();

        let p2 = [("channel", "101"), ("date", "2026-01-30")].into();
        let mut w2 = table.writer(p2).unwrap();
        w2.append(0x01, 1500, b"p2_msg1").unwrap();
        w2.append(0x01, 2500, b"p2_msg2").unwrap();
        w2.finish().unwrap();

        // Query all partitions with timestamp ordering
        let filter = PartitionFilter::All;
        let mut reader = table.query(filter).unwrap();

        assert_eq!(reader.partition_count(), 2);

        // Should read in timestamp order: 1000, 1500, 2000, 2500
        let msg1 = reader.next().unwrap().unwrap();
        assert_eq!(msg1.timestamp_ns, 1000);

        let msg2 = reader.next().unwrap().unwrap();
        assert_eq!(msg2.timestamp_ns, 1500);

        let msg3 = reader.next().unwrap().unwrap();
        assert_eq!(msg3.timestamp_ns, 2000);

        let msg4 = reader.next().unwrap().unwrap();
        assert_eq!(msg4.timestamp_ns, 2500);

        assert!(reader.next().unwrap().is_none());
    }

    #[test]
    fn test_table_query_sequential() {
        let dir = TempDir::new().unwrap();

        let scheme = PartitionScheme::new().add_string("partition");

        let table = Table::create(dir.path(), scheme).unwrap();

        // Write partitions with overlapping timestamps
        let p1 = [("partition", "p1")].into();
        let mut w1 = table.writer(p1).unwrap();
        w1.append(0x01, 3000, b"p1_msg1").unwrap();
        w1.finish().unwrap();

        let p2 = [("partition", "p2")].into();
        let mut w2 = table.writer(p2).unwrap();
        w2.append(0x01, 1000, b"p2_msg1").unwrap();
        w2.finish().unwrap();

        // Sequential read (no timestamp ordering)
        let filter = PartitionFilter::All;
        let mut reader = table
            .query_with_strategy(filter, MergeStrategy::PartitionSequential)
            .unwrap();

        // Reads partitions in discovery order (not timestamp order)
        let msg1 = reader.next().unwrap().unwrap();
        let ts1 = msg1.timestamp_ns;

        let msg2 = reader.next().unwrap().unwrap();
        let ts2 = msg2.timestamp_ns;

        // Both messages read, but order not guaranteed by timestamp
        let timestamps = vec![ts1, ts2];
        assert!(timestamps.contains(&1000));
        assert!(timestamps.contains(&3000));
    }
}
