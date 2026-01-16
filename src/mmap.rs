use std::fs::{File, OpenOptions};
use std::path::Path;

use memmap2::{MmapMut, MmapOptions};

use crate::{Error, Result};

pub struct MmapFile {
    file: File,
    map: MmapMut,
    len: usize,
}

impl MmapFile {
    pub fn create(path: &Path, len: usize) -> Result<Self> {
        if len == 0 {
            return Err(Error::Unsupported("mmap length must be non-zero"));
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        file.set_len(len as u64)?;
        let map = unsafe { MmapOptions::new().len(len).map_mut(&file)? };
        Ok(Self { file, map, len })
    }

    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let len = file.metadata()?.len() as usize;
        if len == 0 {
            return Err(Error::Unsupported("mmap length must be non-zero"));
        }
        let map = unsafe { MmapOptions::new().len(len).map_mut(&file)? };
        Ok(Self { file, map, len })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.map
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.map
    }

    pub fn range_mut(&mut self, offset: usize, len: usize) -> Result<&mut [u8]> {
        let end = offset.checked_add(len).ok_or(Error::Corrupt("range overflow"))?;
        if end > self.len {
            return Err(Error::Corrupt("range out of bounds"));
        }
        Ok(&mut self.map[offset..end])
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn sync(&self) -> Result<()> {
        self.file.sync_all()?;
        Ok(())
    }

    pub fn flush_async(&self) -> Result<()> {
        self.map.flush_async()?;
        Ok(())
    }

    pub fn flush_sync(&self) -> Result<()> {
        self.map.flush()?;
        Ok(())
    }
}
