use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use chronicle_core::header::{MessageHeader, HEADER_SIZE, MAX_PAYLOAD_LEN, PAD_TYPE_ID, RECORD_ALIGN};
use chronicle_core::segment::{SEG_DATA_OFFSET, SEG_MAGIC, SEG_VERSION};
use chronicle_core::zstd_seek::{read_seek_index, ZstdSeekIndex};
use serde::Deserialize;

use crate::resolver::{ResolvedStorage, StorageResolver, StorageTier};

pub struct StorageReader {
    tier: StorageTier,
    source: SegmentSource,
    offset: u64,
    len: u64,
    header_buf: [u8; HEADER_SIZE],
    payload_buf: Vec<u8>,
}

impl StorageReader {
    pub fn open(
        resolver: &StorageResolver,
        venue: &str,
        symbol_code: &str,
        date: &str,
        stream: &str,
    ) -> Result<Self> {
        let resolved = resolver.resolve(venue, symbol_code, date, stream)?;
        match resolved {
            ResolvedStorage::Q { tier, path } => {
                let file = File::open(&path)
                    .with_context(|| format!("open segment {}", path.display()))?;
                let len = file.metadata()?.len();
                let source = SegmentSource::Plain(PlainSegmentSource { file, len });
                build_reader(tier, source, &path)
            }
            ResolvedStorage::Zstd { tier, path, idx_path } => {
                let (source, len) = ZstdSegmentSource::open(&path, &idx_path)?;
                let source = SegmentSource::Zstd(source);
                build_reader(tier, source, &path).map(|mut reader| {
                    reader.len = len;
                    reader
                })
            }
            ResolvedStorage::RemoteStub { stub_path } => {
                let (zst_path, idx_path) =
                    cache_remote_segment(&stub_path, resolver.cache_root())?;
                let (source, len) = ZstdSegmentSource::open(&zst_path, &idx_path)?;
                let source = SegmentSource::Zstd(source);
                build_reader(StorageTier::RemoteCold, source, &zst_path).map(|mut reader| {
                    reader.len = len;
                    reader
                })
            }
        }
    }

    pub fn tier(&self) -> StorageTier {
        self.tier
    }

    pub fn next(&mut self) -> Result<Option<chronicle_core::MessageView<'_>>> {
        loop {
            let start = self.offset;
            let Some((header, payload_len)) = self.read_header_at(start)? else {
                return Ok(None);
            };

            let aligned = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN);
            self.offset = start.saturating_add(aligned as u64);

            if header.type_id == PAD_TYPE_ID {
                continue;
            }

            self.payload_buf.resize(payload_len, 0);
            self.source
                .read_exact_at(start + HEADER_SIZE as u64, &mut self.payload_buf)?;

            return Ok(Some(chronicle_core::MessageView {
                seq: header.seq,
                timestamp_ns: header.timestamp_ns,
                type_id: header.type_id,
                payload: &self.payload_buf,
            }));
        }
    }

    pub fn seek_timestamp(&mut self, target_ts_ns: u64) -> Result<bool> {
        let mut offset = SEG_DATA_OFFSET as u64;
        while let Some((header, payload_len)) = self.read_header_at(offset)? {
            if header.timestamp_ns >= target_ts_ns {
                self.offset = offset;
                return Ok(true);
            }
            let aligned = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN);
            offset = offset.saturating_add(aligned as u64);
        }
        self.offset = self.len;
        Ok(false)
    }

    fn read_header_at(&mut self, offset: u64) -> Result<Option<(MessageHeader, usize)>> {
        if offset + HEADER_SIZE as u64 > self.len {
            return Ok(None);
        }

        self.source.read_exact_at(offset, &mut self.header_buf)?;
        let commit_len = u32::from_le_bytes(self.header_buf[0..4].try_into().expect("slice length"));
        if commit_len == 0 {
            return Ok(None);
        }

        let payload_len = MessageHeader::payload_len_from_commit(commit_len)
            .map_err(|e| anyhow!("invalid commit len: {e}"))?;
        if payload_len > MAX_PAYLOAD_LEN {
            bail!("payload len {} exceeds max", payload_len);
        }

        let header = MessageHeader::from_bytes(&self.header_buf)?;
        
        let end = offset
            .saturating_add(HEADER_SIZE as u64)
            .saturating_add(payload_len as u64);
        if end > self.len {
            bail!("record exceeds segment length");
        }

        Ok(Some((header, payload_len)))
    }
}

fn build_reader(tier: StorageTier, mut source: SegmentSource, path: &Path) -> Result<StorageReader> {
    validate_segment(&mut source, path)?;
    let len = source.len();
    Ok(StorageReader {
        tier,
        source,
        offset: SEG_DATA_OFFSET as u64,
        len,
        header_buf: [0u8; HEADER_SIZE],
        payload_buf: Vec::new(),
    })
}

fn validate_segment(source: &mut SegmentSource, path: &Path) -> Result<()> {
    if source.len() < SEG_DATA_OFFSET as u64 {
        bail!("segment too small: {}", path.display());
    }

    let mut buf = [0u8; SEG_DATA_OFFSET];
    source.read_exact_at(0, &mut buf)?;
    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice length"));
    let version = u32::from_le_bytes(buf[4..8].try_into().expect("slice length"));
    if magic != SEG_MAGIC {
        bail!("segment magic mismatch in {}", path.display());
    }
    if version != SEG_VERSION {
        bail!("segment version mismatch in {}", path.display());
    }
    Ok(())
}

fn align_up(value: usize, alignment: usize) -> usize {
    if alignment == 0 {
        return value;
    }
    (value + alignment - 1) & !(alignment - 1)
}

enum SegmentSource {
    Plain(PlainSegmentSource),
    Zstd(ZstdSegmentSource),
}

impl SegmentSource {
    fn len(&self) -> u64 {
        match self {
            SegmentSource::Plain(source) => source.len,
            SegmentSource::Zstd(source) => source.len,
        }
    }

    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        match self {
            SegmentSource::Plain(source) => source.read_exact_at(offset, buf),
            SegmentSource::Zstd(source) => source.read_exact_at(offset, buf),
        }
    }
}

struct PlainSegmentSource {
    file: File,
    len: u64,
}

impl PlainSegmentSource {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buf)?;
        Ok(())
    }
}

struct ZstdSegmentSource {
    file: File,
    index: ZstdSeekIndex,
    len: u64,
    cached_start: u64,
    cached_block: Vec<u8>,
}

impl ZstdSegmentSource {
    fn open(zst_path: &Path, idx_path: &Path) -> Result<(Self, u64)> {
        let file = File::open(zst_path)
            .with_context(|| format!("open zstd segment {}", zst_path.display()))?;
        let index = read_seek_index(idx_path)
            .with_context(|| format!("read zstd index {}", idx_path.display()))?;
        let len = index
            .entries
            .last()
            .map(|entry| entry.uncompressed_offset + entry.uncompressed_size as u64)
            .unwrap_or(0);
        Ok((
            Self {
                file,
                index,
                len,
                cached_start: 0,
                cached_block: Vec::new(),
            },
            len,
        ))
    }

    fn read_exact_at(&mut self, mut offset: u64, mut buf: &mut [u8]) -> Result<()> {
        while !buf.is_empty() {
            if !self.cache_contains(offset) {
                self.load_block(offset)?;
            }
            let block_offset = (offset - self.cached_start) as usize;
            let available = self.cached_block.len().saturating_sub(block_offset);
            if available == 0 {
                bail!("read beyond cached block");
            }
            let to_copy = available.min(buf.len());
            buf[..to_copy].copy_from_slice(
                &self.cached_block[block_offset..block_offset + to_copy],
            );
            offset = offset.saturating_add(to_copy as u64);
            buf = &mut buf[to_copy..];
        }
        Ok(())
    }

    fn cache_contains(&self, offset: u64) -> bool {
        if self.cached_block.is_empty() {
            return false;
        }
        let end = self
            .cached_start
            .saturating_add(self.cached_block.len() as u64);
        offset >= self.cached_start && offset < end
    }

    fn load_block(&mut self, offset: u64) -> Result<()> {
        let entry = self
            .index
            .entry_for_offset(offset)
            .ok_or_else(|| anyhow!("offset out of range"))?;
        let mut compressed = vec![0u8; entry.compressed_size as usize];
        self.file.seek(SeekFrom::Start(entry.compressed_offset))?;
        self.file.read_exact(&mut compressed)?;
        let decompressed = zstd::stream::decode_all(&compressed[..])
            .context("decompress zstd block")?;
        if decompressed.len() != entry.uncompressed_size as usize {
            bail!(
                "decompressed size mismatch: expected {} got {}",
                entry.uncompressed_size,
                decompressed.len()
            );
        }
        self.cached_start = entry.uncompressed_offset;
        self.cached_block = decompressed;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct RemoteStub {
    remote_uri: String,
    size_bytes: u64,
    idx_size_bytes: u64,
}

fn cache_remote_segment(stub_path: &Path, cache_root: &Path) -> Result<(PathBuf, PathBuf)> {
    let data = std::fs::read(stub_path)
        .with_context(|| format!("read remote stub {}", stub_path.display()))?;
    let stub: RemoteStub = serde_json::from_slice(&data)?;

    let remote_path = path_from_remote_uri(&stub.remote_uri)?;
    let remote_idx = zst_idx_path(&remote_path);

    let cache_dir = cache_root.join(format!("{:016x}", fnv1a64(stub.remote_uri.as_bytes())));
    let file_name = remote_path
        .file_name()
        .ok_or_else(|| anyhow!("remote uri has no filename"))?;
    let cache_zst = cache_dir.join(file_name);
    let cache_idx = zst_idx_path(&cache_zst);

    ensure_cached_file(&remote_path, &cache_zst, stub.size_bytes)?;
    ensure_cached_file(&remote_idx, &cache_idx, stub.idx_size_bytes)?;

    Ok((cache_zst, cache_idx))
}

fn ensure_cached_file(src: &Path, dst: &Path, expected_size: u64) -> Result<()> {
    if let Ok(meta) = std::fs::metadata(dst) {
        if meta.len() == expected_size {
            return Ok(());
        }
    }

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp = dst.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    std::fs::copy(src, &tmp)
        .with_context(|| format!("copy {} -> {}", src.display(), tmp.display()))?;
    let size = std::fs::metadata(&tmp)?.len();
    if size != expected_size {
        bail!(
            "cached file size mismatch: expected {} got {}",
            expected_size,
            size
        );
    }
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

fn path_from_remote_uri(uri: &str) -> Result<PathBuf> {
    if let Some(stripped) = uri.strip_prefix("file://") {
        return Ok(PathBuf::from(stripped));
    }
    if uri.contains("://") {
        bail!("unsupported remote uri scheme: {uri} (only file:// or local paths are currently supported)");
    }
    Ok(PathBuf::from(uri))
}

fn zst_idx_path(zst_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.idx", zst_path.to_string_lossy()))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
