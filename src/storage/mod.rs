pub mod access;
mod archive_writer;
mod meta;
mod raw_archiver;
mod tier;

pub use access::{ResolvedStorage, StorageReader, StorageResolver, StorageTier};
pub use archive_writer::ArchiveWriter;
pub use meta::{read_segment_flags, write_meta_at_if_missing, MetaFile};
pub use raw_archiver::{RawArchiver, RawArchiverConfig};
pub use tier::{TierConfig, TierManager};

// Re-export compression utilities from new location
pub use crate::core::compression::{
    compress_q_to_zst, read_seek_index, write_seek_index, ZstdBlockReader, ZstdSeekEntry,
    ZstdSeekHeader, ZstdSeekIndex, ZSTD_INDEX_ENTRY_LEN, ZSTD_INDEX_HEADER_LEN, ZSTD_INDEX_MAGIC,
    ZSTD_INDEX_VERSION,
};
