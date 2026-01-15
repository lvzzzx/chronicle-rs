use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::header::MessageHeader;
use crate::writer::Queue;
use crate::{Error, Result};

const HEADER_SIZE: usize = 64;
const READERS_DIR: &str = "readers";

pub struct QueueReader {
    queue: Arc<Queue>,
    read_offset: u64,
    name: String,
    meta_path: PathBuf,
}

impl Queue {
    pub fn reader(self: &Arc<Self>, name: &str) -> Result<QueueReader> {
        if name.is_empty() {
            return Err(Error::Unsupported("reader name cannot be empty"));
        }
        let readers_dir = self.path.join(READERS_DIR);
        std::fs::create_dir_all(&readers_dir)?;
        let meta_path = readers_dir.join(format!("{name}.meta"));
        let read_offset = load_read_offset(&meta_path)?;
        Ok(QueueReader {
            queue: Arc::clone(self),
            read_offset,
            name: name.to_string(),
            meta_path,
        })
    }
}

impl QueueReader {
    pub fn next(&mut self) -> Result<Option<Vec<u8>>> {
        let (header, payload, next_offset) = {
            let mmap = self
                .queue
                .mmap
                .lock()
                .map_err(|_| Error::Corrupt("mmap lock poisoned"))?;

            let offset = self.read_offset as usize;
            if offset + HEADER_SIZE > mmap.len() {
                return Ok(None);
            }

            let mut header_buf = [0u8; 64];
            header_buf.copy_from_slice(&mmap.as_slice()[offset..offset + HEADER_SIZE]);
            let header = MessageHeader::from_bytes(&header_buf)?;

            if header.flags & 1 == 0 {
                return Ok(None);
            }

            std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);

            let payload_len = header.length as usize;
            let payload_start = offset + HEADER_SIZE;
            let payload_end = payload_start
                .checked_add(payload_len)
                .ok_or(Error::Corrupt("payload length overflow"))?;
            if payload_end > mmap.len() {
                return Err(Error::Corrupt("payload length out of bounds"));
            }

            let payload = mmap.as_slice()[payload_start..payload_end].to_vec();
            (header, payload, payload_end)
        };
        header.validate_crc(&payload)?;
        self.read_offset = next_offset as u64;
        Ok(Some(payload))
    }

    pub fn commit(&self) -> Result<()> {
        store_read_offset(&self.meta_path, self.read_offset)
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

fn load_read_offset(path: &Path) -> Result<u64> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err.into()),
    };
    let mut buf = [0u8; 8];
    file.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn store_read_offset(path: &Path, offset: u64) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(&offset.to_le_bytes())?;
    file.sync_all()?;
    Ok(())
}
