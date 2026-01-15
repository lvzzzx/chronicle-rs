use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::header::{MessageHeader, FLAGS_OFFSET};
use crate::mmap::MmapFile;
use crate::segment::{load_index, store_index, SegmentIndex, SEGMENT_SIZE};
use crate::{Error, Result};

const SEGMENT_FILE: &str = "000000000.q";
const INDEX_FILE: &str = "index.meta";
const HEADER_SIZE: usize = 64;

pub struct Queue {
    pub(crate) path: PathBuf,
    pub(crate) mmap: Mutex<MmapFile>,
    write_index: AtomicU64,
    current_segment: u64,
}

pub struct QueueWriter {
    queue: Arc<Queue>,
}

impl Queue {
    pub fn open(path: impl AsRef<Path>) -> Result<Arc<Self>> {
        let path = path.as_ref().to_path_buf();
        std::fs::create_dir_all(&path)?;

        let index_path = path.join(INDEX_FILE);
        let index = load_index(&index_path)?;
        if index.current_segment != 0 {
            return Err(Error::Unsupported("only segment 0 supported in phase 2"));
        }
        if index.write_offset as usize > SEGMENT_SIZE {
            return Err(Error::Corrupt("index write_offset exceeds segment size"));
        }

        let segment_path = path.join(SEGMENT_FILE);
        let mmap = if segment_path.exists() {
            let mmap = MmapFile::open(&segment_path)?;
            if mmap.len() != SEGMENT_SIZE {
                return Err(Error::Corrupt("segment size mismatch"));
            }
            mmap
        } else {
            MmapFile::create(&segment_path, SEGMENT_SIZE)?
        };

        Ok(Arc::new(Self {
            path,
            mmap: Mutex::new(mmap),
            write_index: AtomicU64::new(index.write_offset),
            current_segment: index.current_segment,
        }))
    }

    pub fn writer(self: &Arc<Self>) -> QueueWriter {
        QueueWriter {
            queue: Arc::clone(self),
        }
    }
}

impl QueueWriter {
    pub fn append(&self, payload: &[u8]) -> Result<()> {
        let record_len = HEADER_SIZE
            .checked_add(payload.len())
            .ok_or(Error::Unsupported("payload too large"))?;
        let reserve = self
            .queue
            .write_index
            .fetch_add(record_len as u64, Ordering::AcqRel);
        let end = reserve
            .checked_add(record_len as u64)
            .ok_or(Error::Corrupt("write offset overflow"))?;
        let mut mmap = self
            .queue
            .mmap
            .lock()
            .map_err(|_| Error::Corrupt("mmap lock poisoned"))?;
        if end as usize > mmap.len() {
            self.queue
                .write_index
                .fetch_sub(record_len as u64, Ordering::AcqRel);
            return Err(Error::Unsupported("segment full"));
        }

        let checksum = MessageHeader::crc32(payload);
        let header = MessageHeader::new(payload.len() as u32, reserve, 0, 0, checksum);
        let header_bytes = header.to_bytes();
        mmap.range_mut(reserve as usize, HEADER_SIZE)?
            .copy_from_slice(&header_bytes);
        mmap.range_mut((reserve as usize) + HEADER_SIZE, payload.len())?
            .copy_from_slice(payload);

        std::sync::atomic::fence(Ordering::Release);
        mmap.range_mut(reserve as usize + FLAGS_OFFSET, 1)?
            .copy_from_slice(&[1]);
        Ok(())
    }

    pub fn flush(&self) -> Result<()> {
        let index_path = self.queue.path.join(INDEX_FILE);
        let write_offset = self.queue.write_index.load(Ordering::Acquire);
        let index = SegmentIndex::new(self.queue.current_segment, write_offset);
        store_index(&index_path, &index)?;
        Ok(())
    }
}
