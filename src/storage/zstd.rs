use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use crate::core::zstd_seek::{read_seek_index, write_seek_index, ZstdSeekEntry, ZstdSeekIndex};

const DEFAULT_ZSTD_LEVEL: i32 = 3;

pub fn compress_q_to_zst(
    input_path: impl AsRef<Path>,
    output_zst: impl AsRef<Path>,
    output_idx: impl AsRef<Path>,
    block_size: usize,
) -> Result<ZstdSeekIndex> {
    if block_size == 0 {
        return Err(anyhow!("block_size must be > 0"));
    }

    let mut input = File::open(&input_path)
        .with_context(|| format!("open input {}", input_path.as_ref().display()))?;
    let mut output = File::create(&output_zst)
        .with_context(|| format!("create {}", output_zst.as_ref().display()))?;

    let mut buf = vec![0u8; block_size];
    let mut entries: Vec<ZstdSeekEntry> = Vec::new();
    let mut uncompressed_offset: u64 = 0;
    let mut compressed_offset: u64 = 0;

    loop {
        let read_len = input.read(&mut buf)?;
        if read_len == 0 {
            break;
        }

        let compressed =
            zstd::stream::encode_all(&buf[..read_len], DEFAULT_ZSTD_LEVEL)
                .context("compress block")?;
        if compressed.len() > u32::MAX as usize {
            return Err(anyhow!("compressed block exceeds u32 limit"));
        }
        if read_len > u32::MAX as usize {
            return Err(anyhow!("uncompressed block exceeds u32 limit"));
        }

        output.write_all(&compressed)?;

        entries.push(ZstdSeekEntry {
            uncompressed_offset,
            compressed_offset,
            compressed_size: compressed.len() as u32,
            uncompressed_size: read_len as u32,
        });

        uncompressed_offset = uncompressed_offset.saturating_add(read_len as u64);
        compressed_offset = compressed_offset.saturating_add(compressed.len() as u64);
    }

    output.sync_all()?;
    write_seek_index(output_idx.as_ref(), block_size as u32, &entries)?;

    Ok(ZstdSeekIndex {
        block_size: block_size as u32,
        entries,
    })
}

pub struct ZstdBlockReader {
    file: File,
    index: ZstdSeekIndex,
}

impl ZstdBlockReader {
    pub fn open(zst_path: impl AsRef<Path>, idx_path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(&zst_path)
            .with_context(|| format!("open {}", zst_path.as_ref().display()))?;
        let index = read_seek_index(idx_path.as_ref())?;
        Ok(Self { file, index })
    }

    pub fn read_block_at(&mut self, uncompressed_offset: u64) -> Result<Vec<u8>> {
        let entry = self
            .index
            .entry_for_offset(uncompressed_offset)
            .ok_or_else(|| anyhow!("offset out of range"))?;

        let mut compressed = vec![0u8; entry.compressed_size as usize];
        self.file.seek(SeekFrom::Start(entry.compressed_offset))?;
        self.file.read_exact(&mut compressed)?;

        let decompressed = zstd::stream::decode_all(&compressed[..])
            .context("decompress block")?;

        if decompressed.len() != entry.uncompressed_size as usize {
            return Err(anyhow!(
                "decompressed size mismatch: expected {} got {}",
                entry.uncompressed_size,
                decompressed.len()
            ));
        }

        Ok(decompressed)
    }
}

