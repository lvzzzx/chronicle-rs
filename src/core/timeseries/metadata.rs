//! Table metadata management.
//!
//! Handles storage and retrieval of table metadata including partition scheme.

use crate::core::timeseries::PartitionScheme;
use crate::core::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};

const METADATA_VERSION: u32 = 1;
const METADATA_FILENAME: &str = "metadata.json";

/// Table metadata stored in _table/metadata.json.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TableMetadata {
    /// Metadata format version.
    pub version: u32,
    /// Table creation timestamp (nanoseconds).
    pub created_at: u64,
    /// Partition scheme.
    pub scheme: PartitionScheme,
}

impl TableMetadata {
    /// Create new metadata with current timestamp.
    pub fn new(scheme: PartitionScheme) -> Self {
        Self {
            version: METADATA_VERSION,
            created_at: current_timestamp_ns(),
            scheme,
        }
    }

    /// Save metadata to disk.
    pub fn save(&self, table_root: &Path) -> Result<()> {
        let metadata_dir = table_root.join("_table");
        fs::create_dir_all(&metadata_dir)?;

        let metadata_path = metadata_dir.join(METADATA_FILENAME);
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

        fs::write(metadata_path, json)?;
        Ok(())
    }

    /// Load metadata from disk.
    pub fn load(table_root: &Path) -> Result<Self> {
        let metadata_path = metadata_path(table_root);

        if !metadata_path.exists() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Table metadata not found: {}", metadata_path.display()),
            )));
        }

        let json = fs::read_to_string(&metadata_path)?;
        let metadata: TableMetadata = serde_json::from_str(&json)
            .map_err(|_| Error::Corrupt("invalid metadata JSON"))?;

        if metadata.version != METADATA_VERSION {
            return Err(Error::UnsupportedVersion(metadata.version));
        }

        Ok(metadata)
    }

    /// Check if table exists (has metadata file).
    pub fn exists(table_root: &Path) -> bool {
        metadata_path(table_root).exists()
    }
}

/// Get the metadata file path for a table.
pub fn metadata_path(table_root: &Path) -> PathBuf {
    table_root.join("_table").join(METADATA_FILENAME)
}

/// Get current timestamp in nanoseconds.
fn current_timestamp_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::timeseries::PartitionScheme;
    use tempfile::TempDir;

    #[test]
    fn test_metadata_save_load() {
        let dir = TempDir::new().unwrap();

        let scheme = PartitionScheme::new()
            .add_string("channel")
            .add_date("date");

        let metadata = TableMetadata::new(scheme.clone());

        // Save
        metadata.save(dir.path()).unwrap();

        // Load
        let loaded = TableMetadata::load(dir.path()).unwrap();

        assert_eq!(loaded.version, METADATA_VERSION);
        assert_eq!(loaded.scheme, scheme);
        assert!(loaded.created_at > 0);
    }

    #[test]
    fn test_metadata_not_found() {
        let dir = TempDir::new().unwrap();
        let result = TableMetadata::load(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_metadata_exists() {
        let dir = TempDir::new().unwrap();
        assert!(!TableMetadata::exists(dir.path()));

        let scheme = PartitionScheme::new().add_string("key");
        let metadata = TableMetadata::new(scheme);
        metadata.save(dir.path()).unwrap();

        assert!(TableMetadata::exists(dir.path()));
    }
}
