use std::path::{Path, PathBuf};

use anyhow::Result;

mod zstd;
mod tier;
pub mod access;

pub use zstd::{compress_q_to_zst, ZstdBlockReader};
pub use tier::{TierConfig, TierManager};
pub use access::{ResolvedStorage, StorageReader, StorageResolver, StorageTier};