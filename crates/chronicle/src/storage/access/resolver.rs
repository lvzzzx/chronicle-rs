use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageTier {
    Hot,
    Cold,
    RemoteCold,
}

#[derive(Debug, Clone)]
pub enum ResolvedStorage {
    Q {
        tier: StorageTier,
        path: PathBuf,
    },
    Zstd {
        tier: StorageTier,
        path: PathBuf,
        idx_path: PathBuf,
    },
    RemoteStub {
        stub_path: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub struct StorageResolver {
    hot_root: Option<PathBuf>,
    cold_root: Option<PathBuf>,
    cache_root: PathBuf,
}

impl StorageResolver {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            hot_root: Some(root.clone()),
            cold_root: Some(root),
            cache_root: PathBuf::from("/tmp/chronicle-cache"),
        }
    }

    pub fn with_roots(
        hot_root: Option<PathBuf>,
        cold_root: Option<PathBuf>,
    ) -> Self {
        Self {
            hot_root,
            cold_root,
            cache_root: PathBuf::from("/tmp/chronicle-cache"),
        }
    }

    pub fn with_cache_root(mut self, cache_root: impl Into<PathBuf>) -> Self {
        self.cache_root = cache_root.into();
        self
    }

    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    pub fn resolve(
        &self,
        venue: &str,
        symbol_code: &str,
        date: &str,
        stream: &str,
    ) -> Result<ResolvedStorage> {
        let mut seen = HashSet::new();
        let candidates = [
            (StorageTier::Hot, self.hot_root.as_ref()),
            (StorageTier::Cold, self.cold_root.as_ref()),
        ];

        for (tier, root) in candidates {
            let Some(root) = root else { continue };
            if !seen.insert(root.clone()) {
                continue;
            }
            let path = q_path(root, venue, symbol_code, date, stream);
            if path.is_file() {
                return Ok(ResolvedStorage::Q { tier, path });
            }
        }

        if let Some(root) = self
            .cold_root
            .as_ref()
            .or(self.hot_root.as_ref())
        {
            let base = partition_path(root, venue, symbol_code, date);
            let zst_path = base.join(format!("{stream}.q.zst"));
            let idx_path = zst_idx_path(&zst_path);
            if zst_path.is_file() && idx_path.is_file() {
                return Ok(ResolvedStorage::Zstd {
                    tier: StorageTier::Cold,
                    path: zst_path,
                    idx_path,
                });
            }
            let stub_path = base.join(format!("{stream}.q.zst.remote.json"));
            if stub_path.is_file() {
                return Ok(ResolvedStorage::RemoteStub { stub_path });
            }
        }

        bail!(
            "no storage found for venue={} symbol={} date={} stream={}",
            venue,
            symbol_code,
            date,
            stream
        )
    }
}

fn partition_path(root: &Path, venue: &str, symbol_code: &str, date: &str) -> PathBuf {
    root.join("v1").join(venue).join(symbol_code).join(date)
}

fn q_path(root: &Path, venue: &str, symbol_code: &str, date: &str, stream: &str) -> PathBuf {
    partition_path(root, venue, symbol_code, date).join(format!("{stream}.q"))
}

fn zst_idx_path(zst_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.idx", zst_path.to_string_lossy()))
}
