use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::core::segment::SEG_FLAG_SEALED;

use super::compress_q_to_zst;
use super::meta::{read_segment_flags, write_meta_if_missing};

const DEFAULT_BLOCK_SIZE: usize = 1 << 20;

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

pub struct TierManager {
    config: TierConfig,
}

impl TierManager {
    pub fn new(config: TierConfig) -> Self {
        Self { config }
    }

    pub fn run_once(&self) -> Result<()> {
        let mut q_files = Vec::new();
        collect_q_files(&self.config.root, &mut q_files)?;

        for path in q_files {
            self.handle_q_file(&path)?;
        }

        if let Some(remote_root) = &self.config.remote_root {
            let mut zst_files = Vec::new();
            collect_zst_files(&self.config.root, &mut zst_files)?;
            for path in zst_files {
                self.handle_remote_move(&path, remote_root)?;
            }
        }

        Ok(())
    }

    fn handle_q_file(&self, path: &Path) -> Result<()> {
        if !is_eligible_for_cold(path, self.config.cold_after)? {
            return Ok(());
        }

        let flags = read_segment_flags(path)?;
        if (flags & SEG_FLAG_SEALED) == 0 {
            return Ok(());
        }

        let zst_path = path.with_extension("q.zst");
        let idx_path = path.with_extension("q.zst.idx");
        if zst_path.exists() && idx_path.exists() {
            return Ok(());
        }

        let zst_tmp = path.with_extension("q.zst.tmp");
        let idx_tmp = path.with_extension("q.zst.idx.tmp");
        let _ = std::fs::remove_file(&zst_tmp);
        let _ = std::fs::remove_file(&idx_tmp);

        compress_q_to_zst(path, &zst_tmp, &idx_tmp, self.config.block_size)?;

        std::fs::rename(&zst_tmp, &zst_path)?;
        std::fs::rename(&idx_tmp, &idx_path)?;

        write_meta_if_missing(path, &self.config.root)?;

        std::fs::remove_file(path)?;

        Ok(())
    }

    fn handle_remote_move(&self, zst_path: &Path, remote_root: &Path) -> Result<()> {
        if !is_eligible_for_remote(zst_path, self.config.remote_after)? {
            return Ok(());
        }

        let idx_path = zst_path.with_extension("q.zst.idx");
        if !idx_path.exists() {
            return Ok(());
        }

        let stub_path = zst_path.with_extension("q.zst.remote.json");
        if stub_path.exists() {
            return Ok(());
        }

        let relative = zst_path.strip_prefix(&self.config.root).unwrap_or(zst_path);
        let remote_zst = remote_root.join(relative);
        let remote_idx = remote_zst.with_extension("q.zst.idx");

        std::fs::create_dir_all(
            remote_zst
                .parent()
                .ok_or_else(|| anyhow!("remote path has no parent"))?,
        )?;

        let bytes = std::fs::copy(zst_path, &remote_zst)?;
        let idx_bytes = std::fs::copy(&idx_path, &remote_idx)?;

        let stub = RemoteStub {
            remote_uri: remote_zst.to_string_lossy().to_string(),
            size_bytes: bytes,
            idx_size_bytes: idx_bytes,
        };
        write_stub(&stub_path, &stub)?;

        std::fs::remove_file(zst_path)?;
        std::fs::remove_file(idx_path)?;

        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct RemoteStub {
    remote_uri: String,
    size_bytes: u64,
    idx_size_bytes: u64,
}

fn write_stub(path: &Path, stub: &RemoteStub) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(stub)?;
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp)?;
    file.write_all(&data)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn collect_q_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_q_files(&path, files)?;
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.ends_with(".q") && !name.ends_with(".q.tmp") {
                files.push(path);
            }
        }
    }
    Ok(())
}

fn collect_zst_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_zst_files(&path, files)?;
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.ends_with(".q.zst") {
                files.push(path);
            }
        }
    }
    Ok(())
}

fn is_eligible_for_cold(path: &Path, cold_after: Duration) -> Result<bool> {
    if cold_after.as_secs() == 0 {
        return Ok(true);
    }
    let metadata = path.metadata()?;
    let modified = metadata.modified().ok();
    if let Some(modified) = modified {
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::from_secs(0));
        return Ok(age >= cold_after);
    }
    Ok(false)
}

fn is_eligible_for_remote(path: &Path, remote_after: Duration) -> Result<bool> {
    if remote_after.as_secs() == 0 {
        return Ok(false);
    }
    let metadata = path.metadata()?;
    let modified = metadata.modified().ok();
    if let Some(modified) = modified {
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::from_secs(0));
        return Ok(age >= remote_after);
    }
    Ok(false)
}
