use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::core::header::{MessageHeader, HEADER_SIZE, MAX_PAYLOAD_LEN, RECORD_ALIGN};
use crate::core::mmap::MmapFile;
use crate::core::segment::{
    prepare_segment_temp, publish_segment, seal_segment, segment_path, segment_temp_path,
    validate_segment_size, DEFAULT_SEGMENT_SIZE, SEG_DATA_OFFSET,
};
use crate::layout::ArchiveLayout;

pub struct ArchiveWriter {
    stream_dir: PathBuf,
    segment_size: usize,
    segment_id: u64,
    write_offset: u64,
    seq: u64,
    mmap: Option<MmapFile>,
    segments_written: u64,
    has_records: bool,
}

impl ArchiveWriter {
    pub fn new(
        archive_root: impl Into<PathBuf>,
        venue: &str,
        symbol: &str,
        date: &str,
        stream: &str,
        segment_size: usize,
    ) -> Result<Self> {
        let segment_size = if segment_size == 0 {
            DEFAULT_SEGMENT_SIZE
        } else {
            segment_size
        };
        let segment_size = validate_segment_size(segment_size as u64)?;
        let root = archive_root.into();
        let layout = ArchiveLayout::new(&root);
        let stream_dir = layout
            .stream_dir(venue, symbol, date, stream)
            .map_err(|e| anyhow!(e))?;
        std::fs::create_dir_all(&stream_dir)?;

        let segment_id = next_segment_id(&stream_dir)?;
        Ok(Self {
            stream_dir,
            segment_size,
            segment_id,
            write_offset: SEG_DATA_OFFSET as u64,
            seq: 0,
            mmap: None,
            segments_written: 0,
            has_records: false,
        })
    }

    pub fn new_at_dir(stream_dir: impl Into<PathBuf>, segment_size: usize) -> Result<Self> {
        let segment_size = if segment_size == 0 {
            DEFAULT_SEGMENT_SIZE
        } else {
            segment_size
        };
        let segment_size = validate_segment_size(segment_size as u64)?;
        let stream_dir = stream_dir.into();
        std::fs::create_dir_all(&stream_dir)?;
        let segment_id = next_segment_id(&stream_dir)?;
        Ok(Self {
            stream_dir,
            segment_size,
            segment_id,
            write_offset: SEG_DATA_OFFSET as u64,
            seq: 0,
            mmap: None,
            segments_written: 0,
            has_records: false,
        })
    }

    pub fn segments_written(&self) -> u64 {
        self.segments_written
    }

    pub fn append_record(&mut self, type_id: u16, timestamp_ns: u64, payload: &[u8]) -> Result<()> {
        let payload_len = payload.len();
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(anyhow!("payload too large"));
        }

        let record_len = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN);
        let max_payload = self.segment_size.saturating_sub(SEG_DATA_OFFSET);
        if record_len > max_payload {
            return Err(anyhow!("record exceeds segment size"));
        }

        self.ensure_segment()?;

        if (self.write_offset as usize) + record_len > self.segment_size {
            self.roll_segment()?;
            self.ensure_segment()?;
        }

        let offset = self.write_offset as usize;
        let mmap = self
            .mmap
            .as_mut()
            .ok_or_else(|| anyhow!("segment mmap missing"))?;

        if payload_len > 0 {
            mmap.range_mut(offset + HEADER_SIZE, payload_len)?
                .copy_from_slice(payload);
        }

        let checksum = MessageHeader::crc32(payload);
        let header = MessageHeader::new_uncommitted(self.seq, timestamp_ns, type_id, 0, checksum);
        let header_bytes = header.to_bytes();
        mmap.range_mut(offset, HEADER_SIZE)?
            .copy_from_slice(&header_bytes);

        let commit_len = MessageHeader::commit_len_for_payload(payload_len)?;
        let header_ptr = unsafe { mmap.as_mut_slice().as_mut_ptr().add(offset) };
        MessageHeader::store_commit_len(header_ptr, commit_len);

        self.seq = self.seq.wrapping_add(1);
        self.write_offset = self
            .write_offset
            .checked_add(record_len as u64)
            .ok_or_else(|| anyhow!("write offset overflow"))?;
        self.has_records = true;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()> {
        if !self.has_records {
            if let Some(mmap) = self.mmap.take() {
                drop(mmap);
                let temp_path = segment_temp_path(&self.stream_dir, self.segment_id);
                let _ = std::fs::remove_file(temp_path);
            }
            return Ok(());
        }

        self.publish_current()
    }

    fn ensure_segment(&mut self) -> Result<()> {
        if self.mmap.is_some() {
            return Ok(());
        }

        let mmap = prepare_segment_temp(&self.stream_dir, self.segment_id, self.segment_size)?;
        self.write_offset = SEG_DATA_OFFSET as u64;
        self.has_records = false;
        self.mmap = Some(mmap);
        Ok(())
    }

    fn roll_segment(&mut self) -> Result<()> {
        if self.has_records {
            self.publish_current()?;
        } else if let Some(mmap) = self.mmap.take() {
            drop(mmap);
            let temp_path = segment_temp_path(&self.stream_dir, self.segment_id);
            let _ = std::fs::remove_file(temp_path);
        }

        self.segment_id = self.segment_id.saturating_add(1);
        self.write_offset = SEG_DATA_OFFSET as u64;
        self.mmap = None;
        self.has_records = false;
        Ok(())
    }

    fn publish_current(&mut self) -> Result<()> {
        let mut mmap = self
            .mmap
            .take()
            .ok_or_else(|| anyhow!("segment mmap missing"))?;
        seal_segment(&mut mmap)?;
        mmap.flush_async()?;
        let temp_path = segment_temp_path(&self.stream_dir, self.segment_id);
        let final_path = segment_path(&self.stream_dir, self.segment_id);
        publish_segment(&temp_path, &final_path)?;
        self.segments_written = self.segments_written.saturating_add(1);
        Ok(())
    }
}

fn next_segment_id(stream_dir: &Path) -> Result<u64> {
    let mut max_id: Option<u64> = None;
    if stream_dir.exists() {
        for entry in std::fs::read_dir(stream_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            let file_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name,
                None => continue,
            };
            let base = match file_name.split('.').next() {
                Some(base) if !base.is_empty() => base,
                _ => continue,
            };
            if !base.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if let Ok(id) = base.parse::<u64>() {
                max_id = Some(max_id.map_or(id, |cur| cur.max(id)));
            }
        }
    }
    Ok(max_id.map_or(0, |id| id.saturating_add(1)))
}

fn align_up(value: usize, align: usize) -> usize {
    if align == 0 {
        return value;
    }
    (value + align - 1) & !(align - 1)
}
