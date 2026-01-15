use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::header::{MessageHeader, FLAGS_OFFSET};
use crate::mmap::MmapFile;
use crate::notifier::WriterNotifier;
use crate::segment::{
    create_segment, load_index, open_segment, segment_path, store_index, SegmentIndex,
    SEGMENT_SIZE,
};
use crate::{Error, Result};

const INDEX_FILE: &str = "index.meta";
const HEADER_SIZE: usize = 64;

pub struct Queue {
    pub(crate) path: PathBuf,
    pub(crate) mmap: Mutex<MmapFile>,
    write_index: AtomicU64,
    current_segment: AtomicU64,
    pub(crate) notifier: WriterNotifier,
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
        if index.write_offset as usize > SEGMENT_SIZE {
            return Err(Error::Corrupt("index write_offset exceeds segment size"));
        }

        let current_segment = index.current_segment;
        let segment_path = segment_path(&path, current_segment);
        let mmap = if segment_path.exists() {
            open_segment(&path, current_segment)?
        } else {
            create_segment(&path, current_segment)?
        };
        let readers_dir = path.join("readers");
        let notifier = WriterNotifier::new(&readers_dir)?;

        Ok(Arc::new(Self {
            path,
            mmap: Mutex::new(mmap),
            write_index: AtomicU64::new(index.write_offset),
            current_segment: AtomicU64::new(current_segment),
            notifier,
        }))
    }

    pub fn writer(self: &Arc<Self>) -> QueueWriter {
        QueueWriter {
            queue: Arc::clone(self),
        }
    }

    pub fn cleanup(&self) -> Result<Vec<u64>> {
        let current_segment = self.current_segment.load(Ordering::Acquire);
        crate::retention::cleanup_segments(&self.path, current_segment)
    }
}

impl QueueWriter {
    pub fn append(&self, payload: &[u8]) -> Result<()> {
        let record_len = HEADER_SIZE
            .checked_add(payload.len())
            .ok_or(Error::Unsupported("payload too large"))?;
        if record_len > SEGMENT_SIZE {
            return Err(Error::Unsupported("record exceeds segment size"));
        }
        let mut mmap = self
            .queue
            .mmap
            .lock()
            .map_err(|_| Error::Corrupt("mmap lock poisoned"))?;
        let reserve = loop {
            let current = self.queue.write_index.load(Ordering::Acquire);
            let end = current
                .checked_add(record_len as u64)
                .ok_or(Error::Corrupt("write offset overflow"))?;
            if end as usize <= SEGMENT_SIZE {
                if self
                    .queue
                    .write_index
                    .compare_exchange(current, end, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    break current;
                }
                continue;
            }
            self.roll_segment(&mut mmap)?;
        };

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
        self.queue.notifier.notify_all()?;
        Ok(())
    }

    pub fn flush(&self) -> Result<()> {
        let index_path = self.queue.path.join(INDEX_FILE);
        let _lock = self
            .queue
            .mmap
            .lock()
            .map_err(|_| Error::Corrupt("mmap lock poisoned"))?;
        let write_offset = self.queue.write_index.load(Ordering::Acquire);
        let current_segment = self.queue.current_segment.load(Ordering::Acquire);
        let index = SegmentIndex::new(current_segment, write_offset);
        store_index(&index_path, &index)?;
        Ok(())
    }

    fn roll_segment(&self, mmap: &mut MmapFile) -> Result<()> {
        mmap.sync()?;
        let next_segment = self.queue.current_segment.fetch_add(1, Ordering::AcqRel) + 1;
        let new_mmap = create_segment(&self.queue.path, next_segment)?;
        *mmap = new_mmap;
        self.queue.write_index.store(0, Ordering::Release);
        let index_path = self.queue.path.join(INDEX_FILE);
        let index = SegmentIndex::new(next_segment, 0);
        store_index(&index_path, &index)?;
        Ok(())
    }
}
