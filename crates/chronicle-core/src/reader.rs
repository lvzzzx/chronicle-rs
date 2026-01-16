use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::control::ControlFile;
use crate::header::{
    MessageHeader, HEADER_SIZE, MAX_PAYLOAD_LEN, PAD_TYPE_ID, RECORD_ALIGN,
};
use crate::mmap::MmapFile;
use crate::segment::{
    load_reader_meta, open_segment, read_segment_header, segment_path, store_reader_meta,
    ReaderMeta, SEG_DATA_OFFSET, SEG_FLAG_SEALED, SEGMENT_SIZE,
};
use crate::wait::futex_wait;
use crate::writer::Queue;
use crate::{Error, Result};

const READERS_DIR: &str = "readers";
const DEFAULT_SPIN_US: u32 = 10;

pub struct MessageView<'a> {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub type_id: u16,
    pub payload: &'a [u8],
}

pub enum WaitStrategy {
    Hybrid { spin_us: u32 },
    BusyPoll(Duration),
}

pub struct QueueReader {
    path: PathBuf,
    control: ControlFile,
    mmap: MmapFile,
    segment_id: u32,
    read_offset: u64,
    meta_path: PathBuf,
    meta: ReaderMeta,
    wait_strategy: WaitStrategy,
}

impl Queue {
    pub fn open_subscriber(path: impl AsRef<std::path::Path>, reader: &str) -> Result<QueueReader> {
        if reader.is_empty() {
            return Err(Error::Unsupported("reader name cannot be empty"));
        }
        let path = path.as_ref().to_path_buf();
        let control_path = path.join("control.meta");
        let control = ControlFile::open(&control_path)?;
        control.wait_ready()?;

        let readers_dir = path.join(READERS_DIR);
        std::fs::create_dir_all(&readers_dir)?;
        let meta_path = readers_dir.join(format!("{reader}.meta"));
        let mut meta = load_reader_meta(&meta_path)?;
        if meta.offset < SEG_DATA_OFFSET as u64 {
            meta.offset = SEG_DATA_OFFSET as u64;
        }
        meta.last_heartbeat_ns = now_ns()?;
        store_reader_meta(&meta_path, &mut meta)?;

        let segment_path = segment_path(&path, meta.segment_id);
        if !segment_path.exists() {
            return Err(Error::Corrupt("reader segment missing"));
        }
        let mmap = open_segment(&path, meta.segment_id)?;

        Ok(QueueReader {
            path,
            control,
            mmap,
            segment_id: meta.segment_id as u32,
            read_offset: meta.offset,
            meta_path,
            meta,
            wait_strategy: WaitStrategy::Hybrid {
                spin_us: DEFAULT_SPIN_US,
            },
        })
    }
}

impl QueueReader {
    pub fn next(&mut self) -> Result<Option<MessageView<'_>>> {
        let last_possible = (SEGMENT_SIZE - HEADER_SIZE) as u64;
        loop {
            if self.read_offset > last_possible {
                if self.advance_segment()? {
                    continue;
                }
                return Ok(None);
            }

            let offset = self.read_offset as usize;
            let commit = MessageHeader::load_commit_len(&self.mmap.as_slice()[offset] as *const u8);
            if commit == 0 {
                return Ok(None);
            }

            let payload_len = MessageHeader::payload_len_from_commit(commit)?;
            if payload_len > MAX_PAYLOAD_LEN {
                return Err(Error::Corrupt("payload length exceeds max"));
            }
            let record_len = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN);
            if offset + record_len > SEGMENT_SIZE {
                return Err(Error::Corrupt("record length out of bounds"));
            }

            let mut header_buf = [0u8; 64];
            header_buf.copy_from_slice(&self.mmap.as_slice()[offset..offset + HEADER_SIZE]);
            let header = MessageHeader::from_bytes(&header_buf)?;
            self.read_offset = self
                .read_offset
                .checked_add(record_len as u64)
                .ok_or(Error::Corrupt("read offset overflow"))?;

            let payload_start = offset + HEADER_SIZE;
            let payload_ptr = unsafe { self.mmap.as_slice().as_ptr().add(payload_start) };
            let payload = unsafe { std::slice::from_raw_parts(payload_ptr, payload_len) };

            if header.type_id == PAD_TYPE_ID {
                continue;
            }
            header.validate_crc(payload)?;
            return Ok(Some(MessageView {
                seq: header.seq,
                timestamp_ns: header.timestamp_ns,
                type_id: header.type_id,
                payload,
            }));
        }
    }

    pub fn commit(&mut self) -> Result<()> {
        self.meta.segment_id = self.segment_id as u64;
        self.meta.offset = self.read_offset;
        self.meta.last_heartbeat_ns = now_ns()?;
        store_reader_meta(&self.meta_path, &mut self.meta)
    }

    pub fn wait(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.maybe_heartbeat()?;
        match self.wait_strategy {
            WaitStrategy::BusyPoll(duration) => {
                std::thread::sleep(duration);
                return Ok(());
            }
            WaitStrategy::Hybrid { spin_us } => {
                let spin_deadline = std::time::Instant::now()
                    + Duration::from_micros(spin_us as u64);
                while std::time::Instant::now() < spin_deadline {
                    if self.peek_committed()? {
                        return Ok(());
                    }
                    std::hint::spin_loop();
                }
            }
        }
        let seq = self.control.notify_seq().load(Ordering::Acquire);
        if self.peek_committed()? {
            return Ok(());
        }
        futex_wait(self.control.notify_seq(), seq, timeout)
    }

    pub fn set_wait_strategy(&mut self, strategy: WaitStrategy) {
        self.wait_strategy = strategy;
    }

    fn maybe_heartbeat(&mut self) -> Result<()> {
        let now = now_ns()?;
        if now.saturating_sub(self.meta.last_heartbeat_ns) > 1_000_000_000 {
            self.meta.last_heartbeat_ns = now;
            store_reader_meta(&self.meta_path, &mut self.meta)?;
        }
        Ok(())
    }

    fn peek_committed(&self) -> Result<bool> {
        let last_possible = SEGMENT_SIZE - HEADER_SIZE;
        if self.read_offset as usize > last_possible {
            return Ok(false);
        }
        let offset = self.read_offset as usize;
        let commit = MessageHeader::load_commit_len(&self.mmap.as_slice()[offset] as *const u8);
        Ok(commit > 0)
    }

    fn advance_segment(&mut self) -> Result<bool> {
        let header = read_segment_header(&self.mmap)?;
        if (header.flags & SEG_FLAG_SEALED) == 0 {
            return Ok(false);
        }
        let next_segment = self.segment_id + 1;
        let next_path = segment_path(&self.path, next_segment as u64);
        if !next_path.exists() {
            return Ok(false);
        }
        self.mmap = open_segment(&self.path, next_segment as u64)?;
        self.segment_id = next_segment;
        self.read_offset = SEG_DATA_OFFSET as u64;
        Ok(true)
    }
}

fn align_up(value: usize, align: usize) -> usize {
    if align == 0 {
        return value;
    }
    (value + align - 1) & !(align - 1)
}

fn now_ns() -> Result<u64> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Unsupported("system time before UNIX epoch"))?;
    u64::try_from(timestamp.as_nanos())
        .map_err(|_| Error::Unsupported("system time exceeds timestamp range"))
}
