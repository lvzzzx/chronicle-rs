use std::mem::size_of;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::mmap::MmapFile;
use crate::{Error, Result};

pub const CTRL_MAGIC: u32 = 0x4348_524E; // 'CHRN'
pub const CTRL_VERSION: u32 = 1;

#[repr(C, align(128))]
pub struct ControlBlock {
    // Constant / low-frequency fields (avoid sharing with hot fields).
    pub magic: AtomicU32,
    pub version: AtomicU32,
    pub init_state: AtomicU32,
    pub writer_epoch: AtomicU64,
    pub _pad1: [u8; 108],

    // Reader-hot, rarely written.
    pub current_segment: AtomicU32,
    pub _pad2: [u8; 124],

    // Writer-hot.
    pub write_offset: AtomicU64,
    pub notify_seq: AtomicU32,
    pub _pad3: [u8; 116],
}

pub struct ControlFile {
    _mmap: MmapFile,
    ptr: *mut ControlBlock,
}

impl ControlFile {
    pub fn create(path: &Path, current_segment: u32, write_offset: u64, writer_epoch: u64) -> Result<Self> {
        let tmp_path = path.with_extension("tmp");
        let mut mmap = MmapFile::create(&tmp_path, size_of::<ControlBlock>())?;
        mmap.as_mut_slice().fill(0);
        let ptr = mmap.as_mut_slice().as_mut_ptr() as *mut ControlBlock;
        let block = unsafe { &*ptr };
        block.init_state.store(1, Ordering::Relaxed);
        block.version.store(CTRL_VERSION, Ordering::Relaxed);
        block.current_segment.store(current_segment, Ordering::Relaxed);
        block.write_offset.store(write_offset, Ordering::Relaxed);
        block.writer_epoch.store(writer_epoch, Ordering::Relaxed);
        block.magic.store(CTRL_MAGIC, Ordering::Relaxed);
        block.init_state.store(2, Ordering::Release);
        std::fs::rename(tmp_path, path)?;
        Ok(Self { _mmap: mmap, ptr })
    }

    pub fn open(path: &Path) -> Result<Self> {
        let mmap = MmapFile::open(path)?;
        if mmap.len() < size_of::<ControlBlock>() {
            return Err(Error::CorruptMetadata("control.meta too small"));
        }
        let ptr = mmap.as_slice().as_ptr() as *mut ControlBlock;
        Ok(Self { _mmap: mmap, ptr })
    }

    pub fn wait_ready(&self) -> Result<()> {
        let block = self.block();
        loop {
            let state = block.init_state.load(Ordering::Acquire);
            if state == 2 {
                break;
            }
            std::thread::yield_now();
        }
        if block.magic.load(Ordering::Acquire) != CTRL_MAGIC {
            return Err(Error::CorruptMetadata("control.meta magic mismatch"));
        }
        let version = block.version.load(Ordering::Acquire);
        if version != CTRL_VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        Ok(())
    }

    pub fn block(&self) -> &ControlBlock {
        unsafe { &*self.ptr }
    }

    pub fn current_segment(&self) -> u32 {
        self.block().current_segment.load(Ordering::Acquire)
    }

    pub fn set_current_segment(&self, segment: u32) {
        self.block().current_segment.store(segment, Ordering::Release);
    }

    pub fn write_offset(&self) -> u64 {
        self.block().write_offset.load(Ordering::Acquire)
    }

    pub fn set_write_offset(&self, offset: u64) {
        self.block().write_offset.store(offset, Ordering::Release);
    }

    pub fn notify_seq(&self) -> &AtomicU32 {
        &self.block().notify_seq
    }

    pub fn writer_epoch(&self) -> u64 {
        self.block().writer_epoch.load(Ordering::Acquire)
    }

    pub fn set_writer_epoch(&self, epoch: u64) {
        self.block().writer_epoch.store(epoch, Ordering::Release);
    }
}
