//! Core table abstraction.
//!
//! Provides partitioned time-series tables with automatic partition management.

use crate::table::{
    PartitionScheme, PartitionValues, RollingWriter, TableConfig, TableMetadata,
    TimeSeriesReader, TimeSeriesWriter,
};
use crate::table::rollers::PartitionRoller;
use crate::core::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Partition information.
#[derive(Debug, Clone)]
pub struct PartitionInfo {
    /// Partition values.
    pub values: PartitionValues,
    /// Partition directory path.
    pub path: PathBuf,
}

/// Time-series table with partition support.
///
/// A table is a collection of partitioned time-series logs organized
/// in a directory structure following Hive-style conventions.
///
/// # Example
///
/// ```no_run
/// use chronicle::core::timeseries::{Table, PartitionScheme};
///
/// let scheme = PartitionScheme::new()
///     .add_string("channel")
///     .add_date("date");
///
/// let table = Table::create("./l3_data", scheme)?;
///
/// // Write to specific partition
/// let partition = [("channel", "101"), ("date", "2026-01-29")].into();
/// let mut writer = table.writer(partition)?;
/// writer.append(0x01, timestamp_ns, &data)?;
/// writer.finish()?;
/// # Ok::<(), chronicle::core::Error>(())
/// ```
pub struct Table {
    root: PathBuf,
    metadata: TableMetadata,
    config: TableConfig,
}

impl Table {
    /// Create a new table with the given partition scheme.
    ///
    /// # Errors
    ///
    /// - `Error::Io`: Failed to create table directory
    /// - `Error::TableExists`: Table already exists at this path
    pub fn create(path: impl AsRef<Path>, scheme: PartitionScheme) -> Result<Self> {
        Self::create_with_config(path, scheme, TableConfig::default())
    }

    /// Create a new table with custom configuration.
    pub fn create_with_config(
        path: impl AsRef<Path>,
        scheme: PartitionScheme,
        config: TableConfig,
    ) -> Result<Self> {
        let root = path.as_ref().to_path_buf();

        // Check if table already exists
        if TableMetadata::exists(&root) {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("Table already exists at {}", root.display()),
            )));
        }

        // Create root directory
        fs::create_dir_all(&root)?;

        // Save metadata
        let metadata = TableMetadata::new(scheme);
        metadata.save(&root)?;

        Ok(Self {
            root,
            metadata,
            config,
        })
    }

    /// Open an existing table.
    ///
    /// # Errors
    ///
    /// - `Error::Io`: Failed to read table directory or metadata
    /// - `Error::Corrupt`: Invalid metadata format
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_config(path, TableConfig::default())
    }

    /// Open an existing table with custom configuration.
    pub fn open_with_config(path: impl AsRef<Path>, config: TableConfig) -> Result<Self> {
        let root = path.as_ref().to_path_buf();

        // Load metadata
        let metadata = TableMetadata::load(&root)?;

        Ok(Self {
            root,
            metadata,
            config,
        })
    }

    /// Get the partition scheme.
    pub fn scheme(&self) -> &PartitionScheme {
        &self.metadata.scheme
    }

    /// Get the table root path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the table configuration.
    pub fn config(&self) -> &TableConfig {
        &self.config
    }

    /// Create a writer for a specific partition.
    ///
    /// # Errors
    ///
    /// - `Error::InvalidPartition`: Partition values don't match scheme
    /// - `Error::Io`: Failed to create partition directory
    /// - `Error::WriterLockHeld`: Another writer is active for this partition
    pub fn writer(&self, partition: PartitionValues) -> Result<PartitionWriter> {
        // Validate partition values
        self.metadata.scheme.validate(&partition)?;

        // Create partition directory
        let partition_path = self.partition_path(&partition);
        fs::create_dir_all(&partition_path)?;

        // Create TimeSeriesWriter
        let writer = TimeSeriesWriter::open_with_config(
            &partition_path,
            self.config.segment_size,
            self.config.index_stride,
        )?;

        Ok(PartitionWriter { writer })
    }

    /// Create a TimeSeriesWriter for a partition (internal use by RollingWriter).
    pub(crate) fn create_ts_writer(&self, partition: PartitionValues) -> Result<TimeSeriesWriter> {
        // Validate partition values
        self.metadata.scheme.validate(&partition)?;

        // Create partition directory
        let partition_path = self.partition_path(&partition);
        fs::create_dir_all(&partition_path)?;

        // Create TimeSeriesWriter
        TimeSeriesWriter::open_with_config(
            &partition_path,
            self.config.segment_size,
            self.config.index_stride,
        )
    }

    /// Create a rolling writer with a custom partition roller.
    ///
    /// The roller determines partition values from message timestamp and payload.
    pub fn rolling_writer(
        &self,
        roller: impl PartitionRoller + 'static,
    ) -> Result<RollingWriter> {
        RollingWriter::new(self.clone(), Box::new(roller))
    }

    /// Create a rolling writer that partitions by date.
    ///
    /// # Arguments
    ///
    /// * `date_key` - Name of the date partition key in the scheme
    /// * `static_values` - Static partition values (e.g., channel)
    ///
    /// # Example
    ///
    /// ```no_run
    /// use chronicle::core::timeseries::Table;
    ///
    /// let table = Table::open("./l3_data")?;
    /// let mut writer = table.rolling_writer_by_date("date",
    ///     [("channel", "101")].into())?;
    ///
    /// writer.append(0x01, timestamp_ns, &data)?;
    /// writer.finish()?;
    /// # Ok::<(), chronicle::core::Error>(())
    /// ```
    pub fn rolling_writer_by_date(
        &self,
        date_key: &str,
        static_values: PartitionValues,
    ) -> Result<RollingWriter> {
        use crate::table::{DateRoller, Timezone};

        // Find date key in scheme
        let date_key_def = self
            .metadata
            .scheme
            .keys()
            .iter()
            .find(|k| k.name() == date_key)
            .ok_or_else(|| {
                Error::InvalidPartition(format!("Date key '{}' not found in scheme", date_key))
            })?;

        // Extract timezone from date key
        let timezone = match date_key_def {
            crate::table::PartitionKey::Date { timezone, .. } => {
                timezone.as_deref().unwrap_or("UTC")
            }
            _ => {
                return Err(Error::InvalidPartition(format!(
                    "Key '{}' is not a Date partition key",
                    date_key
                )))
            }
        };

        let roller = DateRoller::new(
            self.metadata.scheme.clone(),
            date_key,
            Timezone::from_str(timezone)?,
            static_values,
        );

        self.rolling_writer(roller)
    }

    /// Create a reader for a specific partition.
    ///
    /// # Errors
    ///
    /// - `Error::InvalidPartition`: Partition values don't match scheme
    /// - `Error::Io`: Failed to read partition directory
    pub fn reader(&self, partition: PartitionValues) -> Result<TimeSeriesReader> {
        // Validate partition values
        self.metadata.scheme.validate(&partition)?;

        // Open TimeSeriesReader
        let partition_path = self.partition_path(&partition);
        TimeSeriesReader::open(&partition_path)
    }

    /// List all partitions in the table.
    ///
    /// Scans the table directory structure to discover existing partitions.
    pub fn partitions(&self) -> Result<Vec<PartitionInfo>> {
        let mut partitions = Vec::new();
        self.scan_partitions(&self.root, &mut PartitionValues::new(), &mut partitions)?;
        Ok(partitions)
    }

    /// Recursively scan for partitions.
    fn scan_partitions(
        &self,
        dir: &Path,
        current_values: &mut PartitionValues,
        result: &mut Vec<PartitionInfo>,
    ) -> Result<()> {
        let depth = current_values.len();

        // If we've matched all partition keys, this is a partition
        if depth == self.metadata.scheme.keys().len() {
            result.push(PartitionInfo {
                values: current_values.clone(),
                path: dir.to_path_buf(),
            });
            return Ok(());
        }

        // Scan subdirectories
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            // Skip metadata directory
            if path.file_name() == Some(std::ffi::OsStr::new("_table")) {
                continue;
            }

            if path.is_dir() {
                // Parse partition segment (key=value)
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if let Some((key, value)) = name.split_once('=') {
                        // Parse value based on partition key type
                        let partition_value =
                            self.parse_partition_value(key, value, depth)?;

                        current_values.insert(key.to_string(), partition_value);
                        self.scan_partitions(&path, current_values, result)?;

                        // Remove after recursion
                        current_values.0.remove(key);
                    }
                }
            }
        }

        Ok(())
    }

    /// Parse a partition value based on key type.
    fn parse_partition_value(
        &self,
        key: &str,
        value: &str,
        depth: usize,
    ) -> Result<crate::table::PartitionValue> {
        use crate::table::{PartitionKey, PartitionValue};

        let key_def = self
            .metadata
            .scheme
            .keys()
            .get(depth)
            .ok_or_else(|| Error::InvalidPartition(format!("Unexpected partition key: {}", key)))?;

        if key_def.name() != key {
            return Err(Error::InvalidPartition(format!(
                "Expected key '{}' at depth {}, found '{}'",
                key_def.name(),
                depth,
                key
            )));
        }

        match key_def {
            PartitionKey::String { .. } => Ok(PartitionValue::String(value.to_string())),
            PartitionKey::Int { .. } => {
                let i = value.parse::<i64>().map_err(|_| {
                    Error::InvalidPartition(format!("Invalid integer value: {}", value))
                })?;
                Ok(PartitionValue::Int(i))
            }
            PartitionKey::Date { .. }
            | PartitionKey::Hour { .. }
            | PartitionKey::Minute { .. } => Ok(PartitionValue::String(value.to_string())),
        }
    }

    /// Get partition directory path.
    fn partition_path(&self, partition: &PartitionValues) -> PathBuf {
        self.root.join(partition.to_path(&self.metadata.scheme))
    }
}

// Implement Clone for Table
impl Clone for Table {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            metadata: self.metadata.clone(),
            config: self.config.clone(),
        }
    }
}

/// Writer for a specific partition.
///
/// Thin wrapper around `TimeSeriesWriter` that enforces partition constraints.
pub struct PartitionWriter {
    writer: TimeSeriesWriter,
}

impl PartitionWriter {
    /// Append a record to this partition.
    pub fn append(&mut self, type_id: u16, timestamp_ns: u64, payload: &[u8]) -> Result<()> {
        self.writer.append(type_id, timestamp_ns, payload)
    }

    /// Flush pending writes.
    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush()
    }

    /// Finish writing and seal the partition.
    pub fn finish(&mut self) -> Result<()> {
        self.writer.finish()
    }

    /// Get the current sequence number.
    pub fn seq(&self) -> u64 {
        self.writer.seq()
    }

    /// Get the current segment ID.
    pub fn segment_id(&self) -> u64 {
        self.writer.segment_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::PartitionScheme;
    use tempfile::TempDir;

    #[test]
    fn test_table_create_and_open() {
        let dir = TempDir::new().unwrap();

        let scheme = PartitionScheme::new()
            .add_string("channel")
            .add_date("date");

        // Create table
        let table = Table::create(dir.path(), scheme.clone()).unwrap();
        assert_eq!(table.scheme(), &scheme);

        // Reopen table
        let table2 = Table::open(dir.path()).unwrap();
        assert_eq!(table2.scheme(), &scheme);
    }

    #[test]
    fn test_table_write_partition() {
        let dir = TempDir::new().unwrap();

        let scheme = PartitionScheme::new()
            .add_string("channel")
            .add_date("date");

        let table = Table::create(dir.path(), scheme).unwrap();

        // Write to partition
        let partition = [("channel", "101"), ("date", "2026-01-29")].into();
        let mut writer = table.writer(partition).unwrap();
        writer.append(0x01, 1000, b"test").unwrap();
        writer.finish().unwrap();

        // Verify partition directory exists
        let partition_path = dir.path().join("channel=101/date=2026-01-29");
        assert!(partition_path.exists());
    }

    #[test]
    fn test_table_list_partitions() {
        let dir = TempDir::new().unwrap();

        let scheme = PartitionScheme::new()
            .add_string("channel")
            .add_date("date");

        let table = Table::create(dir.path(), scheme).unwrap();

        // Write to multiple partitions
        let p1 = [("channel", "101"), ("date", "2026-01-29")].into();
        let mut w1 = table.writer(p1).unwrap();
        w1.append(0x01, 1000, b"test").unwrap();
        w1.finish().unwrap();

        let p2 = [("channel", "102"), ("date", "2026-01-29")].into();
        let mut w2 = table.writer(p2).unwrap();
        w2.append(0x01, 2000, b"test").unwrap();
        w2.finish().unwrap();

        // List partitions
        let partitions = table.partitions().unwrap();
        assert_eq!(partitions.len(), 2);
    }

    #[test]
    fn test_table_invalid_partition() {
        let dir = TempDir::new().unwrap();

        let scheme = PartitionScheme::new().add_string("channel");
        let table = Table::create(dir.path(), scheme).unwrap();

        // Try to write with missing key
        let partition = [("date", "2026-01-29")].into();
        let result = table.writer(partition);
        assert!(result.is_err());
    }
}
