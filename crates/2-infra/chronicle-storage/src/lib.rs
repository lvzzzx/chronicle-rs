use std::path::{Path, PathBuf};

use anyhow::Result;

mod zstd;
mod tier;

pub use zstd::{compress_q_to_zst, ZstdBlockReader};
pub use tier::{TierConfig, TierManager};

