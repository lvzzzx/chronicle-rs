use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use time::OffsetDateTime;

use crate::core::segment::{
    load_reader_meta, store_reader_meta, ReaderMeta, SEG_DATA_OFFSET, SEG_FLAG_SEALED,
};
use crate::layout::RawArchiveLayout;
use crate::storage::compress_q_to_zst;
use crate::storage::meta::{
    read_segment_flags, scan_segment_timestamps, write_meta_at_if_missing, MetaFile,
};

const READERS_DIR: &str = "readers";
const DEFAULT_BLOCK_SIZE: usize = 1 << 20;

#[derive(Debug, Clone)]
pub struct RawArchiverConfig {
    pub source: PathBuf,
    pub archive_root: PathBuf,
    pub venue: String,
    pub reader_name: String,
    pub compress: bool,
    pub retain_q: bool,
    pub block_size: usize,
    pub poll_interval: Duration,
}

impl RawArchiverConfig {
    pub fn new(
        source: impl Into<PathBuf>,
        archive_root: impl Into<PathBuf>,
        venue: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            archive_root: archive_root.into(),
            venue: venue.into(),
            reader_name: "raw-archiver".to_string(),
            compress: false,
            retain_q: false,
            block_size: DEFAULT_BLOCK_SIZE,
            poll_interval: Duration::from_secs(1),
        }
    }
}

pub struct RawArchiver {
    config: RawArchiverConfig,
}

impl RawArchiver {
    pub fn new(config: RawArchiverConfig) -> Self {
        Self { config }
    }

    pub fn run_once(&mut self) -> Result<()> {
        if !self.config.source.exists() {
            return Ok(());
        }

        let reader_meta_path = self.reader_meta_path()?;
        let mut meta = load_reader_meta(&reader_meta_path)?;

        let mut segments = list_segment_ids(&self.config.source)?;
        if segments.is_empty() {
            heartbeat(&reader_meta_path, &mut meta)?;
            return Ok(());
        }
        segments.sort_unstable();

        let start_id = match segments.iter().copied().find(|id| *id >= meta.segment_id) {
            Some(id) => id,
            None => {
                heartbeat(&reader_meta_path, &mut meta)?;
                return Ok(());
            }
        };
        if start_id != meta.segment_id {
            meta.segment_id = start_id;
            meta.offset = SEG_DATA_OFFSET as u64;
            meta.last_heartbeat_ns = now_ns();
            store_reader_meta(&reader_meta_path, &mut meta)?;
        }

        let layout = RawArchiveLayout::new(&self.config.archive_root);
        let mut advanced = false;

        let start_segment = meta.segment_id;
        for segment_id in segments.into_iter().filter(|id| *id >= start_segment) {
            let src_path = segment_path(&self.config.source, segment_id);
            if !src_path.exists() {
                continue;
            }

            let flags = read_segment_flags(&src_path)?;
            if (flags & SEG_FLAG_SEALED) == 0 {
                break;
            }

            let (event_time_range, date) = partition_date(&src_path)?;
            let dest_dir = layout
                .raw_dir(&self.config.venue, &date)
                .map_err(|e| anyhow!(e))?;
            std::fs::create_dir_all(&dest_dir)?;

            let dest_q = layout
                .raw_segment_path(&self.config.venue, &date, segment_id)
                .map_err(|e| anyhow!(e))?;
            let dest_zst = layout
                .raw_compressed_segment_path(&self.config.venue, &date, segment_id)
                .map_err(|e| anyhow!(e))?;
            let dest_idx = layout
                .raw_compressed_index_path(&self.config.venue, &date, segment_id)
                .map_err(|e| anyhow!(e))?;
            let meta_path = layout
                .raw_meta_path(&self.config.venue, &date)
                .map_err(|e| anyhow!(e))?;

            if dest_q.exists() || dest_zst.exists() {
                let meta_file = build_meta(&self.config.venue, event_time_range);
                write_meta_at_if_missing(&meta_path, &meta_file)?;
                advance_reader(&reader_meta_path, &mut meta, segment_id)?;
                advanced = true;
                continue;
            }

            copy_segment(&src_path, &dest_q)?;

            if self.config.compress {
                compress_segment(&dest_q, &dest_zst, &dest_idx, self.config.block_size)?;
                if !self.config.retain_q {
                    let _ = std::fs::remove_file(&dest_q);
                }
            }

            let meta_file = build_meta(&self.config.venue, event_time_range);
            write_meta_at_if_missing(&meta_path, &meta_file)?;

            advance_reader(&reader_meta_path, &mut meta, segment_id)?;
            advanced = true;
        }

        if !advanced {
            heartbeat(&reader_meta_path, &mut meta)?;
        }

        Ok(())
    }

    pub fn run_loop(&mut self) -> Result<()> {
        loop {
            self.run_once()?;
            std::thread::sleep(self.config.poll_interval);
        }
    }

    fn reader_meta_path(&self) -> Result<PathBuf> {
        let readers_dir = self.config.source.join(READERS_DIR);
        std::fs::create_dir_all(&readers_dir)?;
        Ok(readers_dir.join(format!("{}.meta", self.config.reader_name)))
    }
}

fn list_segment_ids(root: &Path) -> Result<Vec<u64>> {
    let mut segments = Vec::new();
    if !root.exists() {
        return Ok(segments);
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("q") {
            continue;
        }
        let stem = match path.file_stem().and_then(|name| name.to_str()) {
            Some(name) => name,
            None => continue,
        };
        if let Ok(id) = stem.parse::<u64>() {
            segments.push(id);
        }
    }
    Ok(segments)
}

fn segment_path(root: &Path, segment_id: u64) -> PathBuf {
    root.join(format!("{:09}.q", segment_id))
}

fn partition_date(path: &Path) -> Result<(Option<[u64; 2]>, String)> {
    let event_time_range = scan_segment_timestamps(path)?;
    if let Some([start, _]) = event_time_range {
        if let Ok(date) = date_from_timestamp_ns(start) {
            return Ok((event_time_range, date));
        }
    }

    let fallback_time = path
        .metadata()
        .and_then(|meta| meta.modified())
        .unwrap_or_else(|_| SystemTime::now());
    let date = format_date(OffsetDateTime::from(fallback_time));
    Ok((event_time_range, date))
}

fn date_from_timestamp_ns(timestamp_ns: u64) -> Result<String> {
    let dt = OffsetDateTime::from_unix_timestamp_nanos(timestamp_ns as i128)
        .map_err(|err| anyhow!("timestamp out of range: {err}"))?;
    Ok(format_date(dt))
}

fn format_date(dt: OffsetDateTime) -> String {
    let date = dt.date();
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        date.month() as u8,
        date.day()
    )
}

fn build_meta(venue: &str, event_time_range: Option<[u64; 2]>) -> MetaFile {
    MetaFile {
        symbol_code: None,
        venue: Some(venue.to_string()),
        ingest_time_ns: now_ns(),
        event_time_range,
        completeness: "sealed".to_string(),
    }
}

fn copy_segment(src: &Path, dest: &Path) -> Result<()> {
    let tmp = tmp_path_for(dest)?;
    let _ = std::fs::remove_file(&tmp);

    let mut input = File::open(src).with_context(|| format!("open source {}", src.display()))?;
    let mut output = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp)
        .with_context(|| format!("create {}", tmp.display()))?;

    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    std::fs::rename(&tmp, dest)?;
    fsync_dir(
        dest.parent()
            .ok_or_else(|| anyhow!("archive has no parent"))?,
    )?;
    Ok(())
}

fn compress_segment(src: &Path, dest_zst: &Path, dest_idx: &Path, block_size: usize) -> Result<()> {
    if dest_zst.exists() && dest_idx.exists() {
        return Ok(());
    }

    let zst_tmp = tmp_path_for(dest_zst)?;
    let idx_tmp = tmp_path_for(dest_idx)?;
    let _ = std::fs::remove_file(&zst_tmp);
    let _ = std::fs::remove_file(&idx_tmp);

    compress_q_to_zst(src, &zst_tmp, &idx_tmp, block_size)?;

    std::fs::rename(&zst_tmp, dest_zst)?;
    std::fs::rename(&idx_tmp, dest_idx)?;
    fsync_dir(
        dest_zst
            .parent()
            .ok_or_else(|| anyhow!("archive has no parent"))?,
    )?;
    Ok(())
}

fn tmp_path_for(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("missing filename for {}", path.display()))?
        .to_string_lossy();
    Ok(path.with_file_name(format!("{name}.tmp")))
}

fn fsync_dir(path: &Path) -> Result<()> {
    let dir = File::open(path)?;
    dir.sync_all()?;
    Ok(())
}

fn advance_reader(meta_path: &Path, meta: &mut ReaderMeta, segment_id: u64) -> Result<()> {
    meta.segment_id = segment_id + 1;
    meta.offset = SEG_DATA_OFFSET as u64;
    meta.last_heartbeat_ns = now_ns();
    store_reader_meta(meta_path, meta)?;
    Ok(())
}

fn heartbeat(meta_path: &Path, meta: &mut ReaderMeta) -> Result<()> {
    meta.last_heartbeat_ns = now_ns();
    store_reader_meta(meta_path, meta)?;
    Ok(())
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
