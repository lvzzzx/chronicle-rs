use std::path::PathBuf;
use std::sync::Arc;

use crate::header::MessageHeader;
use crate::mmap::MmapFile;
use crate::notifier::ReaderNotifier;
use crate::segment::{
    load_reader_position, open_segment, segment_path, store_reader_position, ReaderPosition,
};
use crate::writer::Queue;
use crate::{Error, Result};

const HEADER_SIZE: usize = 64;
const READERS_DIR: &str = "readers";

pub struct QueueMessage {
    pub header: MessageHeader,
    pub payload: Vec<u8>,
}

pub struct QueueReader {
    queue: Arc<Queue>,
    mmap: MmapFile,
    segment_id: u64,
    read_offset: u64,
    name: String,
    meta_path: PathBuf,
    notifier: ReaderNotifier,
}

impl Queue {
    pub fn reader(self: &Arc<Self>, name: &str) -> Result<QueueReader> {
        if name.is_empty() {
            return Err(Error::Unsupported("reader name cannot be empty"));
        }
        let readers_dir = self.path.join(READERS_DIR);
        std::fs::create_dir_all(&readers_dir)?;
        let meta_path = readers_dir.join(format!("{name}.meta"));
        let position = load_reader_position(&meta_path)?;
        let segment_path = segment_path(&self.path, position.segment_id);
        if !segment_path.exists() {
            return Err(Error::Corrupt("reader segment missing"));
        }
        let mmap = open_segment(&self.path, position.segment_id)?;
        let notifier = ReaderNotifier::new(&readers_dir, name)?;
        Ok(QueueReader {
            queue: Arc::clone(self),
            mmap,
            segment_id: position.segment_id,
            read_offset: position.offset,
            name: name.to_string(),
            meta_path,
            notifier,
        })
    }
}

impl QueueReader {
    pub fn next(&mut self) -> Result<Option<Vec<u8>>> {
        Ok(self.next_message()?.map(|message| message.payload))
    }

    pub fn next_message(&mut self) -> Result<Option<QueueMessage>> {
        let (header, payload, next_offset) = loop {
            let offset = self.read_offset as usize;
            if offset + HEADER_SIZE > self.mmap.len() {
                if self.advance_segment()? {
                    continue;
                }
                return Ok(None);
            }

            let mut header_buf = [0u8; 64];
            header_buf.copy_from_slice(&self.mmap.as_slice()[offset..offset + HEADER_SIZE]);
            let header = MessageHeader::from_bytes(&header_buf)?;

            if header.flags & 1 == 0 {
                if header.length == 0 && self.advance_segment()? {
                    continue;
                }
                return Ok(None);
            }

            std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);

            let payload_len = header.length as usize;
            let payload_start = offset + HEADER_SIZE;
            let payload_end = payload_start
                .checked_add(payload_len)
                .ok_or(Error::Corrupt("payload length overflow"))?;
            if payload_end > self.mmap.len() {
                return Err(Error::Corrupt("payload length out of bounds"));
            }

            let payload = self.mmap.as_slice()[payload_start..payload_end].to_vec();
            break (header, payload, payload_end);
        };
        header.validate_crc(&payload)?;
        self.read_offset = next_offset as u64;
        Ok(Some(QueueMessage { header, payload }))
    }

    pub fn commit(&self) -> Result<()> {
        let position = ReaderPosition::new(self.segment_id, self.read_offset);
        store_reader_position(&self.meta_path, &position)
    }

    pub fn wait(&self) -> Result<()> {
        self.notifier.wait()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn advance_segment(&mut self) -> Result<bool> {
        let next_segment = self.segment_id + 1;
        let next_path = segment_path(&self.queue.path, next_segment);
        if !next_path.exists() {
            return Ok(false);
        }
        match std::fs::metadata(&next_path) {
            Ok(metadata) => {
                if metadata.len() != crate::segment::SEGMENT_SIZE as u64 {
                    return Ok(false);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err.into()),
        }
        self.mmap = open_segment(&self.queue.path, next_segment)?;
        self.segment_id = next_segment;
        self.read_offset = 0;
        Ok(true)
    }
}
