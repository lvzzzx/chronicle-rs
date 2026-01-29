//! Storage tiering for hot/cold/remote data.
//!
//! **Refactored** to use the unified `StorageLifecycleManager`.
//!
//! This module now provides a compatibility layer over the new lifecycle management system.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::lifecycle::{LifecycleConfig, StorageLifecycleManager};
use crate::table::CompressionPolicy;

const DEFAULT_BLOCK_SIZE: usize = 1 << 20;

/// Configuration for tiered storage.
///
/// **Note**: This is a compatibility wrapper. For new code, use
/// `lifecycle::LifecycleConfig` directly.
#[derive(Debug, Clone)]
pub struct TierConfig {
    pub root: PathBuf,
    pub block_size: usize,
    pub cold_after: Duration,
    pub remote_after: Duration,
    pub remote_root: Option<PathBuf>,
}

impl TierConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            block_size: DEFAULT_BLOCK_SIZE,
            cold_after: Duration::from_secs(0),
            remote_after: Duration::from_secs(0),
            remote_root: None,
        }
    }
}

/// Manages storage tiering (hot → cold → remote).
///
/// **Refactored**: Now delegates to `StorageLifecycleManager`.
pub struct TierManager {
    lifecycle_manager: StorageLifecycleManager,
    config: TierConfig,
}

impl TierManager {
    pub fn new(config: TierConfig) -> Self {
        // Convert TierConfig to LifecycleConfig
        let policy = if config.cold_after.as_secs() == 0 {
            CompressionPolicy::Immediate
        } else {
            CompressionPolicy::AgeAfter {
                days: (config.cold_after.as_secs() / 86400) as u32,
            }
        };

        let lifecycle_config = LifecycleConfig {
            policy,
            compression_level: 3,
            block_size: config.block_size,
            parallel_workers: 1,
        };

        let lifecycle_manager =
            StorageLifecycleManager::new(&config.root, lifecycle_config)
                .expect("Failed to create lifecycle manager");

        Self {
            lifecycle_manager,
            config,
        }
    }

    /// Run tier management once.
    ///
    /// Compresses segments to cold storage based on configuration.
    /// Remote tiering is not yet implemented.
    pub fn run_once(&mut self) -> Result<()> {
        let stats = self.lifecycle_manager.run_once()?;

        if stats.has_errors() {
            eprintln!("Tier management errors: {:?}", stats.errors);
        }

        if self.config.remote_root.is_some() {
            // TODO: Implement remote tiering using lifecycle system
            eprintln!("Remote tiering not yet implemented in new system");
        }

        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct RemoteStub {
    remote_uri: String,
    size_bytes: u64,
    idx_size_bytes: u64,
}
