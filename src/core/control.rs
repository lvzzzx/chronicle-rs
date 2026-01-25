use std::mem::size_of;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::core::mmap::MmapFile;
use crate::core::{Error, Result};

pub const CTRL_MAGIC: u32 = 0x4348_524E; // 'CHRN'
pub const CTRL_VERSION: u32 = 3;

#[repr(C, align(128))]
pub struct ControlBlock {
    // Constant / low-frequency fields (avoid sharing with hot fields).
    pub magic: AtomicU32,
    pub version: AtomicU32,
    pub init_state: AtomicU32,
    pub _pad0: [u8; 4],
    pub writer_epoch: AtomicU64,
    pub segment_size: AtomicU64,
    pub _pad1: [u8; 96],

    // Reader-hot, rarely written.
    pub segment_gen: AtomicU32,
    pub current_segment: AtomicU32,
    pub _pad2: [u8; 120],

    // Writer-hot.
    pub write_offset: AtomicU64,
    pub writer_heartbeat_ns: AtomicU64,
    pub _pad3: [u8; 112],

    // Coordination (Writer-Notify / Reader-Wait).
    // Separated from write_offset to avoid false sharing when readers update waiters_pending.
    pub notify_seq: AtomicU32,
    pub waiters_pending: AtomicU32,
    pub _pad4: [u8; 120],
}

pub struct ControlFile {
    _mmap: MmapFile,
    ptr: *mut ControlBlock,
}

// SAFETY: ControlFile owns the mapping and raw pointer; moving it to another
// thread is safe as long as it is not concurrently accessed.
unsafe impl Send for ControlFile {}

impl ControlFile {
    pub fn create(
        path: &Path,
        current_segment: u32,
        write_offset: u64,
        writer_epoch: u64,
        segment_size: u64,
    ) -> Result<Self> {
        let tmp_path = path.with_extension("tmp");
        let mut mmap = MmapFile::create(&tmp_path, size_of::<ControlBlock>())?;
        mmap.as_mut_slice().fill(0);
        let ptr = mmap.as_mut_slice().as_mut_ptr() as *mut ControlBlock;
        let block = unsafe { &*ptr };
        block.init_state.store(1, Ordering::Relaxed);
        block.version.store(CTRL_VERSION, Ordering::Relaxed);
        block.segment_size.store(segment_size, Ordering::Relaxed);
        block.segment_gen.store(0, Ordering::Relaxed);
        block
            .current_segment
            .store(current_segment, Ordering::Relaxed);
        block.write_offset.store(write_offset, Ordering::Relaxed);
        block.writer_epoch.store(writer_epoch, Ordering::Relaxed);
        block.writer_heartbeat_ns.store(0, Ordering::Relaxed);
        block.notify_seq.store(0, Ordering::Relaxed);
        block.waiters_pending.store(0, Ordering::Relaxed);
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

    pub fn lock(&self) -> Result<()> {
        self._mmap.lock()
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

    pub fn check_ready(&self) -> Result<bool> {
        let block = self.block();
        let state = block.init_state.load(Ordering::Acquire);
        if state != 2 {
            return Ok(false);
        }
        if block.magic.load(Ordering::Acquire) != CTRL_MAGIC {
            return Err(Error::CorruptMetadata("control.meta magic mismatch"));
        }
        let version = block.version.load(Ordering::Acquire);
        if version != CTRL_VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        Ok(true)
    }

    pub fn block(&self) -> &ControlBlock {
        unsafe { &*self.ptr }
    }

    pub fn current_segment(&self) -> u32 {
        self.block().current_segment.load(Ordering::Acquire)
    }

    pub fn segment_size(&self) -> u64 {
        self.block().segment_size.load(Ordering::Acquire)
    }

    pub fn write_offset(&self) -> u64 {
        self.block().write_offset.load(Ordering::Acquire)
    }

    pub(crate) fn set_write_offset(&self, offset: u64) {
        self.block().write_offset.store(offset, Ordering::Release);
    }

    pub fn segment_index(&self) -> (u32, u64) {
        loop {
            let start = self.block().segment_gen.load(Ordering::Acquire);
            if (start & 1) != 0 {
                std::hint::spin_loop();
                continue;
            }
            let segment = self.block().current_segment.load(Ordering::Acquire);
            let offset = self.block().write_offset.load(Ordering::Acquire);
            let end = self.block().segment_gen.load(Ordering::Acquire);
            if start == end && (end & 1) == 0 {
                return (segment, offset);
            }
        }
    }

    pub fn set_segment_index(&self, segment: u32, offset: u64) {
        self.block().segment_gen.fetch_add(1, Ordering::SeqCst);
        self.block()
            .current_segment
            .store(segment, Ordering::Relaxed);
        self.block().write_offset.store(offset, Ordering::Relaxed);
        self.block().segment_gen.fetch_add(1, Ordering::SeqCst);
    }

    pub fn notify_seq(&self) -> &AtomicU32 {
        &self.block().notify_seq
    }

    pub fn waiters_pending(&self) -> &AtomicU32 {
        &self.block().waiters_pending
    }

    pub fn writer_heartbeat_ns(&self) -> u64 {
        self.block().writer_heartbeat_ns.load(Ordering::Acquire)
    }

    pub fn set_writer_heartbeat_ns(&self, heartbeat: u64) {
        self.block()
            .writer_heartbeat_ns
            .store(heartbeat, Ordering::Release);
    }

    pub fn writer_epoch(&self) -> u64 {
        self.block().writer_epoch.load(Ordering::Acquire)
    }

    pub fn set_writer_epoch(&self, epoch: u64) {
        self.block().writer_epoch.store(epoch, Ordering::Release);
    }
}
