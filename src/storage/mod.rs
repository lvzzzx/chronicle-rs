pub mod access;
mod archive_writer;
mod meta;
mod raw_archiver;
mod tier;
mod zstd;
pub mod zstd_seek;

pub use access::{ResolvedStorage, StorageReader, StorageResolver, StorageTier};
pub use archive_writer::ArchiveWriter;
pub use meta::{read_segment_flags, write_meta_at_if_missing, MetaFile};
pub use raw_archiver::{RawArchiver, RawArchiverConfig};
pub use tier::{TierConfig, TierManager};
pub use zstd::{compress_q_to_zst, ZstdBlockReader};
pub use zstd_seek::{
    read_seek_index, write_seek_index, ZstdSeekEntry, ZstdSeekHeader, ZstdSeekIndex,
    ZSTD_INDEX_ENTRY_LEN, ZSTD_INDEX_HEADER_LEN, ZSTD_INDEX_MAGIC, ZSTD_INDEX_VERSION,
};
