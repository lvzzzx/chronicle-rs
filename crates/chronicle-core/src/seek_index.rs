use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::{Error, Result};

pub(crate) const INDEX_MAGIC: [u8; 8] = *b"CHRIDX1\0";
pub(crate) const INDEX_VERSION: u16 = 1;
pub(crate) const INDEX_HEADER_LEN: u16 = 80;
pub(crate) const INDEX_FLAG_PARTIAL: u32 = 1 << 0;
pub(crate) const DEFAULT_INDEX_STRIDE_RECORDS: u32 = 4096;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeekIndexHeader {
    pub magic: [u8; 8],
    pub version: u16,
    pub header_len: u16,
    pub flags: u32,
    pub segment_id: u64,
    pub segment_size: u64,
    pub data_offset: u32,
    pub entry_stride: u32,
    pub min_seq: u64,
    pub max_seq: u64,
    pub min_ts_ns: u64,
    pub max_ts_ns: u64,
    pub entry_count: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeekIndexEntry {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub offset: u64,
}

pub(crate) struct SeekIndexBuilder {
    segment_id: u64,
    segment_size: u64,
    data_offset: u32,
    entry_stride: u32,
    record_index: u64,
    next_entry_at: u64,
    min_seq: Option<u64>,
    max_seq: Option<u64>,
    min_ts_ns: Option<u64>,
    max_ts_ns: Option<u64>,
    entries: Vec<SeekIndexEntry>,
    partial: bool,
}

impl SeekIndexBuilder {
    pub(crate) fn new(
        segment_id: u64,
        segment_size: u64,
        data_offset: u32,
        entry_stride: u32,
    ) -> Self {
        Self {
            segment_id,
            segment_size,
            data_offset,
            entry_stride: entry_stride.max(1),
            record_index: 0,
            next_entry_at: 0,
            min_seq: None,
            max_seq: None,
            min_ts_ns: None,
            max_ts_ns: None,
            entries: Vec::new(),
            partial: false,
        }
    }

    pub(crate) fn mark_partial(&mut self) {
        self.partial = true;
    }

    pub(crate) fn observe(&mut self, seq: u64, timestamp_ns: u64, offset: u64) {
        self.min_seq = Some(self.min_seq.map_or(seq, |cur| cur.min(seq)));
        self.max_seq = Some(self.max_seq.map_or(seq, |cur| cur.max(seq)));
        self.min_ts_ns = Some(self.min_ts_ns.map_or(timestamp_ns, |cur| cur.min(timestamp_ns)));
        self.max_ts_ns = Some(self.max_ts_ns.map_or(timestamp_ns, |cur| cur.max(timestamp_ns)));

        if self.record_index == self.next_entry_at {
            self.entries.push(SeekIndexEntry {
                seq,
                timestamp_ns,
                offset,
            });
            self.next_entry_at = self
                .next_entry_at
                .saturating_add(self.entry_stride as u64);
        }
        self.record_index = self.record_index.saturating_add(1);
    }

    pub(crate) fn reset(&mut self, segment_id: u64, segment_size: u64) {
        self.segment_id = segment_id;
        self.segment_size = segment_size;
        self.record_index = 0;
        self.next_entry_at = 0;
        self.min_seq = None;
        self.max_seq = None;
        self.min_ts_ns = None;
        self.max_ts_ns = None;
        self.entries.clear();
        self.partial = false;
    }

    pub(crate) fn flush(&self, root: &Path) -> Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }

        let header = SeekIndexHeader {
            magic: INDEX_MAGIC,
            version: INDEX_VERSION,
            header_len: INDEX_HEADER_LEN,
            flags: if self.partial { INDEX_FLAG_PARTIAL } else { 0 },
            segment_id: self.segment_id,
            segment_size: self.segment_size,
            data_offset: self.data_offset,
            entry_stride: self.entry_stride,
            min_seq: self.min_seq.unwrap_or(0),
            max_seq: self.max_seq.unwrap_or(0),
            min_ts_ns: self.min_ts_ns.unwrap_or(0),
            max_ts_ns: self.max_ts_ns.unwrap_or(0),
            entry_count: self.entries.len() as u64,
        };

        let path = index_path(root, self.segment_id);
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .map_err(Error::Io)?;
        let header_bytes = encode_header(&header);
        file.write_all(&header_bytes).map_err(Error::Io)?;
        for entry in &self.entries {
            let mut buf = [0u8; 24];
            buf[0..8].copy_from_slice(&entry.seq.to_le_bytes());
            buf[8..16].copy_from_slice(&entry.timestamp_ns.to_le_bytes());
            buf[16..24].copy_from_slice(&entry.offset.to_le_bytes());
            file.write_all(&buf).map_err(Error::Io)?;
        }
        file.sync_all().map_err(Error::Io)?;
        Ok(())
    }
}

pub(crate) fn index_filename(id: u64) -> String {
    format!("{:09}.idx", id)
}

pub(crate) fn index_path(root: &Path, id: u64) -> PathBuf {
    root.join(index_filename(id))
}

pub(crate) fn load_index_header(root: &Path, segment_id: u64) -> Result<Option<SeekIndexHeader>> {
    let path = index_path(root, segment_id);
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Error::Io(err)),
    };
    let mut buf = [0u8; INDEX_HEADER_LEN as usize];
    file.read_exact(&mut buf).map_err(Error::Io)?;
    let header = decode_header(&buf)?;
    Ok(Some(header))
}

pub(crate) fn load_index_entries(
    root: &Path,
    header: &SeekIndexHeader,
) -> Result<Vec<SeekIndexEntry>> {
    let path = index_path(root, header.segment_id);
    let mut file = std::fs::File::open(&path).map_err(Error::Io)?;
    file.seek(SeekFrom::Start(header.header_len as u64))
        .map_err(Error::Io)?;
    let count = usize::try_from(header.entry_count)
        .map_err(|_| Error::Corrupt("seek index entry count overflow"))?;
    let mut entries = Vec::with_capacity(count);
    let mut buf = [0u8; 24];
    for _ in 0..count {
        file.read_exact(&mut buf).map_err(Error::Io)?;
        let seq = u64::from_le_bytes(buf[0..8].try_into().expect("slice length"));
        let timestamp_ns = u64::from_le_bytes(buf[8..16].try_into().expect("slice length"));
        let offset = u64::from_le_bytes(buf[16..24].try_into().expect("slice length"));
        entries.push(SeekIndexEntry {
            seq,
            timestamp_ns,
            offset,
        });
    }
    Ok(entries)
}

fn encode_header(header: &SeekIndexHeader) -> [u8; INDEX_HEADER_LEN as usize] {
    let mut buf = [0u8; INDEX_HEADER_LEN as usize];
    buf[0..8].copy_from_slice(&header.magic);
    buf[8..10].copy_from_slice(&header.version.to_le_bytes());
    buf[10..12].copy_from_slice(&header.header_len.to_le_bytes());
    buf[12..16].copy_from_slice(&header.flags.to_le_bytes());
    buf[16..24].copy_from_slice(&header.segment_id.to_le_bytes());
    buf[24..32].copy_from_slice(&header.segment_size.to_le_bytes());
    buf[32..36].copy_from_slice(&header.data_offset.to_le_bytes());
    buf[36..40].copy_from_slice(&header.entry_stride.to_le_bytes());
    buf[40..48].copy_from_slice(&header.min_seq.to_le_bytes());
    buf[48..56].copy_from_slice(&header.max_seq.to_le_bytes());
    buf[56..64].copy_from_slice(&header.min_ts_ns.to_le_bytes());
    buf[64..72].copy_from_slice(&header.max_ts_ns.to_le_bytes());
    buf[72..80].copy_from_slice(&header.entry_count.to_le_bytes());
    buf
}

fn decode_header(buf: &[u8]) -> Result<SeekIndexHeader> {
    if buf.len() < INDEX_HEADER_LEN as usize {
        return Err(Error::Corrupt("seek index header too small"));
    }
    if buf[0..8] != INDEX_MAGIC {
        return Err(Error::Corrupt("seek index magic mismatch"));
    }
    let version = u16::from_le_bytes(buf[8..10].try_into().expect("slice length"));
    if version != INDEX_VERSION {
        return Err(Error::UnsupportedVersion(version as u32));
    }
    let header_len = u16::from_le_bytes(buf[10..12].try_into().expect("slice length"));
    if header_len != INDEX_HEADER_LEN {
        return Err(Error::Corrupt("seek index header length mismatch"));
    }
    let flags = u32::from_le_bytes(buf[12..16].try_into().expect("slice length"));
    let segment_id = u64::from_le_bytes(buf[16..24].try_into().expect("slice length"));
    let segment_size = u64::from_le_bytes(buf[24..32].try_into().expect("slice length"));
    let data_offset = u32::from_le_bytes(buf[32..36].try_into().expect("slice length"));
    let entry_stride = u32::from_le_bytes(buf[36..40].try_into().expect("slice length"));
    let min_seq = u64::from_le_bytes(buf[40..48].try_into().expect("slice length"));
    let max_seq = u64::from_le_bytes(buf[48..56].try_into().expect("slice length"));
    let min_ts_ns = u64::from_le_bytes(buf[56..64].try_into().expect("slice length"));
    let max_ts_ns = u64::from_le_bytes(buf[64..72].try_into().expect("slice length"));
    let entry_count = u64::from_le_bytes(buf[72..80].try_into().expect("slice length"));
    Ok(SeekIndexHeader {
        magic: INDEX_MAGIC,
        version,
        header_len,
        flags,
        segment_id,
        segment_size,
        data_offset,
        entry_stride,
        min_seq,
        max_seq,
        min_ts_ns,
        max_ts_ns,
        entry_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn seek_index_round_trip() -> Result<()> {
        let dir = tempdir().map_err(Error::Io)?;
        let mut builder = SeekIndexBuilder::new(7, 4096, 64, 2);
        for i in 0..5_u64 {
            builder.observe(100 + i, 1_000 + i, 64 + i * 64);
        }
        builder.flush(dir.path())?;

        let header = load_index_header(dir.path(), 7)?.expect("missing index header");
        assert_eq!(header.segment_id, 7);
        assert_eq!(header.entry_count, 3);
        assert_eq!(header.min_seq, 100);
        assert_eq!(header.max_seq, 104);

        let entries = load_index_entries(dir.path(), &header)?;
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].seq, 100);
        assert_eq!(entries[1].seq, 102);
        assert_eq!(entries[2].seq, 104);
        Ok(())
    }
}
