use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::control::ControlFile;
use crate::header::{MessageHeader, HEADER_SIZE, MAX_PAYLOAD_LEN, RECORD_ALIGN};
use crate::mmap::MmapFile;
use crate::segment::{
    create_segment, load_index, open_or_create_segment, open_segment, prepare_segment_temp,
    publish_segment, read_segment_header, seal_segment, segment_path, segment_temp_path,
    store_index, validate_segment_size, DEFAULT_SEGMENT_SIZE, SegmentIndex, SEG_DATA_OFFSET,
    SEG_FLAG_SEALED,
};
use crate::wait::futex_wake;
use crate::writer_lock;
use crate::{Error, Result};

const INDEX_FILE: &str = "index.meta";
const CONTROL_FILE: &str = "control.meta";
const WRITER_LOCK_FILE: &str = "writer.lock";
const BACKPRESSURE_POLL_US: u64 = 100;
const RETENTION_CHECK_INTERVAL_MS: u64 = 10;
const RETENTION_CHECK_BYTES: u64 = 1024 * 1024;
const PREALLOC_SPIN_ITERS: u32 = 64;
const PREALLOC_YIELD_EVERY: u32 = 64;
const PREALLOC_QUEUE_DEPTH: usize = 1;
const PREALLOC_NO_REQUEST: u64 = u64::MAX;

// Avoid scanning reader metadata on every append; refresh on coarse thresholds.
struct RetentionCache {
    min_pos: u64,
    last_head: u64,
    last_check: Instant,
    valid: bool,
    check_interval: Duration,
    check_bytes: u64,
}

impl RetentionCache {
    fn new(check_interval: Duration, check_bytes: u64) -> Self {
        Self {
            min_pos: 0,
            last_head: 0,
            last_check: Instant::now(),
            valid: false,
            check_interval,
            check_bytes,
        }
    }

    fn min_pos(
        &mut self,
        path: &Path,
        head_segment: u64,
        head_offset: u64,
        segment_size: u64,
    ) -> Result<u64> {
        let head = head_segment
            .saturating_mul(segment_size)
            .saturating_add(head_offset);
        let now = Instant::now();
        if !self.valid
            || now.duration_since(self.last_check) >= self.check_interval
            || head.saturating_sub(self.last_head) >= self.check_bytes
        {
            let min_pos = crate::retention::min_live_reader_position(
                path,
                head_segment,
                head_offset,
                segment_size,
            )?;
            self.min_pos = min_pos;
            self.last_head = head;
            self.last_check = now;
            self.valid = true;
        }
        Ok(self.min_pos)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum BackpressurePolicy {
    FailFast,
    Block { timeout: Option<Duration>, poll_interval: Duration },
}

#[derive(Clone, Copy, Debug)]
pub struct WriterConfig {
    pub max_segments: Option<u64>,
    pub max_bytes: Option<u64>,
    pub backpressure: BackpressurePolicy,
    pub segment_size_bytes: u64,
    pub retention_check_interval: Duration,
    pub retention_check_bytes: u64,
    // Offload segment sync to a background thread on roll (lower latency, not durable to power loss).
    pub defer_seal_sync: bool,
    // Spin-wait budget for a preallocated next segment during roll.
    pub prealloc_wait: Duration,
    // If true, roll errors when no preallocated segment is ready after prealloc_wait.
    pub require_prealloc: bool,
    // Lock control + active segment pages in RAM (requires CAP_IPC_LOCK or high memlock rlimit).
    pub memlock: bool,
}

impl Default for WriterConfig {
    fn default() -> Self {
        Self {
            max_segments: None,
            max_bytes: None,
            backpressure: BackpressurePolicy::FailFast,
            segment_size_bytes: DEFAULT_SEGMENT_SIZE as u64,
            retention_check_interval: Duration::from_millis(RETENTION_CHECK_INTERVAL_MS),
            retention_check_bytes: RETENTION_CHECK_BYTES,
            defer_seal_sync: false,
            prealloc_wait: Duration::from_micros(0),
            require_prealloc: false,
            memlock: false,
        }
    }
}

impl WriterConfig {
    pub fn blocking(max_segments: Option<u64>, max_bytes: Option<u64>, timeout: Option<Duration>) -> Self {
        Self {
            max_segments,
            max_bytes,
            backpressure: BackpressurePolicy::Block {
                timeout,
                poll_interval: Duration::from_micros(BACKPRESSURE_POLL_US),
            },
            segment_size_bytes: DEFAULT_SEGMENT_SIZE as u64,
            retention_check_interval: Duration::from_millis(RETENTION_CHECK_INTERVAL_MS),
            retention_check_bytes: RETENTION_CHECK_BYTES,
            defer_seal_sync: false,
            prealloc_wait: Duration::from_micros(0),
            require_prealloc: false,
            memlock: false,
        }
    }

    pub fn ultra_low_latency() -> Self {
        Self {
            defer_seal_sync: true,
            prealloc_wait: Duration::from_millis(1),
            require_prealloc: true,
            memlock: true,
            ..Self::default()
        }
    }
}

pub struct Queue;

struct PreparedSegment {
    segment_id: u64,
    mmap: MmapFile,
}

struct PreallocWorker {
    wakeups: mpsc::SyncSender<()>,
    desired: Arc<AtomicU64>,
    ready: mpsc::Receiver<PreparedSegment>,
    errors: Arc<AtomicU64>,
}

impl PreallocWorker {
    fn new(root: PathBuf, segment_size: usize, memlock: bool) -> Result<Self> {
        let (wakeup_tx, wakeup_rx) = mpsc::sync_channel(PREALLOC_QUEUE_DEPTH);
        let (ready_tx, ready_rx) = mpsc::sync_channel(PREALLOC_QUEUE_DEPTH);
        let errors = Arc::new(AtomicU64::new(0));
        let errors_handle = Arc::clone(&errors);
        let desired = Arc::new(AtomicU64::new(PREALLOC_NO_REQUEST));
        let desired_handle = Arc::clone(&desired);
        thread::Builder::new()
            .name("segment-prealloc".to_string())
            .spawn(move || {
                while wakeup_rx.recv().is_ok() {
                    let next_segment_id =
                        desired_handle.swap(PREALLOC_NO_REQUEST, Ordering::AcqRel);
                    if next_segment_id == PREALLOC_NO_REQUEST {
                        continue;
                    }
                    let temp_path = segment_temp_path(&root, next_segment_id);
                    let mmap = match prepare_segment_temp(&root, next_segment_id, segment_size) {
                        Ok(mmap) => mmap,
                        Err(_) => {
                            let _ = std::fs::remove_file(&temp_path);
                            errors_handle.fetch_add(1, Ordering::Relaxed);
                            thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                    };
                    if publish_segment(&temp_path, &segment_path(&root, next_segment_id)).is_ok() {
                        if memlock {
                            if mmap.lock().is_err() {
                                errors_handle.fetch_add(1, Ordering::Relaxed);
                                thread::sleep(Duration::from_millis(10));
                                continue;
                            }
                        }
                        if ready_tx
                            .send(PreparedSegment {
                                segment_id: next_segment_id,
                                mmap,
                            })
                            .is_err()
                        {
                            break;
                        }
                    } else {
                        let _ = std::fs::remove_file(&temp_path);
                        errors_handle.fetch_add(1, Ordering::Relaxed);
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            })
            .map_err(Error::Io)?;
        Ok(Self {
            wakeups: wakeup_tx,
            desired,
            ready: ready_rx,
            errors,
        })
    }

    fn request(&self, next_segment_id: u64) {
        self.desired.store(next_segment_id, Ordering::Release);
        let _ = self.wakeups.try_send(());
    }

    fn try_take(&self, next_segment: u64) -> Result<Option<MmapFile>> {
        loop {
            match self.ready.try_recv() {
                Ok(prepared) => {
                    if prepared.segment_id == next_segment {
                        return Ok(Some(prepared.mmap));
                    }
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "prealloc worker closed",
                    )))
                }
            }
        }
    }

    fn error_count(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }
}

struct AsyncSealer {
    sender: mpsc::Sender<MmapFile>,
    errors: Arc<AtomicU64>,
}

impl AsyncSealer {
    fn new() -> Result<Self> {
        let (sender, receiver) = mpsc::channel::<MmapFile>();
        let errors = Arc::new(AtomicU64::new(0));
        let errors_handle = Arc::clone(&errors);
        thread::Builder::new()
            .name("segment-sync".to_string())
            .spawn(move || {
                while let Ok(mmap) = receiver.recv() {
                    if mmap.sync().is_err() {
                        errors_handle.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
            .map_err(Error::Io)?;
        Ok(Self { sender, errors })
    }

    fn submit(&self, mmap: MmapFile) -> Result<()> {
        self.sender
            .send(mmap)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "async sealer closed"))
            .map_err(Error::Io)
    }

    fn error_count(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }
}

pub struct QueueWriter {
    path: PathBuf,
    control: ControlFile,
    mmap: MmapFile,
    segment_id: u32,
    write_offset: u64,
    seq: u64,
    config: WriterConfig,
    segment_size: usize,
    retention_cache: RetentionCache,
    prealloc: PreallocWorker,
    async_sealer: Option<AsyncSealer>,
    _lock: WriterLock,
}

impl Queue {
    pub fn open_publisher(path: impl AsRef<Path>) -> Result<QueueWriter> {
        Self::open_publisher_with_config(path, WriterConfig::default())
    }

    pub fn open_publisher_with_config(
        path: impl AsRef<Path>,
        config: WriterConfig,
    ) -> Result<QueueWriter> {
        let path = path.as_ref().to_path_buf();
        std::fs::create_dir_all(&path)?;

        let control_path = path.join(CONTROL_FILE);
        let lock = WriterLock::acquire(&path.join(WRITER_LOCK_FILE), 0)?;

        let (control, segment_size) = if control_path.exists() {
            let control = ControlFile::open(&control_path)?;
            control.wait_ready()?;
            let segment_size = validate_segment_size(control.segment_size())?;
            (control, segment_size)
        } else {
            let segment_size = validate_segment_size(config.segment_size_bytes)?;
            let control = ControlFile::create(
                &control_path,
                0,
                SEG_DATA_OFFSET as u64,
                0,
                config.segment_size_bytes,
            )?;
            (control, segment_size)
        };
        let writer_epoch = control.writer_epoch().wrapping_add(1).max(1);
        control.set_writer_epoch(writer_epoch);
        lock.update_epoch(writer_epoch)?;
        control.set_writer_heartbeat_ns(now_ns()?);

        let (control_segment, control_offset) = control.segment_index();

        let index_path = path.join(INDEX_FILE);
        let index = load_index(&index_path)?;
        let mut segment_id = index.current_segment as u32;
        let mut scan_offset = index.write_offset as usize;

        if control_segment > segment_id {
            let prev_segment = control_segment - 1;
            let prev_path = segment_path(&path, prev_segment as u64);
            if prev_path.exists() {
                let mut prev_mmap = open_segment(&path, prev_segment as u64, segment_size)?;
                let prev_header = read_segment_header(&prev_mmap)?;
                if (prev_header.flags & SEG_FLAG_SEALED) == 0 {
                    crate::segment::repair_unsealed_tail(&mut prev_mmap, segment_size)?;
                }
            }
            segment_id = control_segment;
            scan_offset = control_offset as usize;
        }
        let mut mmap = if segment_path(&path, segment_id as u64).exists() {
            open_segment(&path, segment_id as u64, segment_size)?
        } else {
            create_segment(&path, segment_id as u64, segment_size)?
        };

        let (tail_offset, tail_partial) =
            scan_segment_tail(&mmap, scan_offset, segment_size)?;
        let mut write_offset = tail_offset as u64;

        if tail_partial {
            if let Some(next_segment) =
                repair_tail_and_roll(&mut mmap, &path, segment_id, segment_size)?
            {
                segment_id = next_segment;
                mmap = open_segment(&path, segment_id as u64, segment_size)?;
                write_offset = SEG_DATA_OFFSET as u64;
            } else {
                write_offset = SEG_DATA_OFFSET as u64;
            }
        }

        let header = read_segment_header(&mmap)?;
        let next_segment_id = segment_id + 1;
        let next_path = segment_path(&path, next_segment_id as u64);
        if !tail_partial && (header.flags & SEG_FLAG_SEALED) != 0 {
            if next_path.exists() {
                segment_id = next_segment_id;
                mmap = open_segment(&path, segment_id as u64, segment_size)?;
            } else {
                mmap = create_segment(&path, next_segment_id as u64, segment_size)?;
                segment_id = next_segment_id;
            }
            write_offset = SEG_DATA_OFFSET as u64;
        }

        control.set_segment_index(segment_id, write_offset);

        let prealloc = PreallocWorker::new(path.clone(), segment_size, config.memlock)?;
        let async_sealer = if config.defer_seal_sync {
            Some(AsyncSealer::new()?)
        } else {
            None
        };
        if config.memlock {
            control.lock()?;
            mmap.lock()?;
        }
        let writer = QueueWriter {
            path,
            control,
            mmap,
            segment_id,
            write_offset,
            seq: 0,
            config,
            segment_size,
            retention_cache: RetentionCache::new(config.retention_check_interval, config.retention_check_bytes),
            prealloc,
            async_sealer,
            _lock: lock,
        };

        writer.trigger_preallocation(segment_id as u64 + 1);
        Ok(writer)
    }
}

impl QueueWriter {
    fn take_preallocated(&mut self, next_segment: u64) -> Result<Option<MmapFile>> {
        self.prealloc.try_take(next_segment)
    }

    fn acquire_preallocated(&mut self, next_segment: u64) -> Result<Option<MmapFile>> {
        if let Some(mmap) = self.take_preallocated(next_segment)? {
            return Ok(Some(mmap));
        }

        if self.config.prealloc_wait.is_zero() {
            if self.config.require_prealloc {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "preallocated segment not ready",
                )));
            }
            return Ok(None);
        }

        let deadline = Instant::now() + self.config.prealloc_wait;

        let mut spins = 0_u32;
        loop {
            for _ in 0..PREALLOC_SPIN_ITERS {
                std::hint::spin_loop();
            }

            if let Some(mmap) = self.take_preallocated(next_segment)? {
                return Ok(Some(mmap));
            }

            spins = spins.wrapping_add(1);
            if spins % PREALLOC_YIELD_EVERY == 0 {
                thread::yield_now();
            }

            if Instant::now() >= deadline {
                break;
            }
        }

        if self.config.require_prealloc {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "preallocated segment not ready",
            )));
        }
        Ok(None)
    }

    fn trigger_preallocation(&self, next_segment_id: u64) {
        self.prealloc.request(next_segment_id);
    }

    pub fn segment_id(&self) -> u32 {
        self.segment_id
    }

    pub fn append(&mut self, type_id: u16, payload: &[u8]) -> Result<()> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Unsupported("system time before UNIX epoch"))?;
        let timestamp_ns = u64::try_from(timestamp.as_nanos())
            .map_err(|_| Error::Unsupported("system time exceeds timestamp range"))?;
        self.append_with_timestamp(type_id, payload, timestamp_ns)
    }

    pub fn append_with_timestamp(
        &mut self,
        type_id: u16,
        payload: &[u8],
        timestamp_ns: u64,
    ) -> Result<()> {
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(Error::PayloadTooLarge);
        }
        let record_len = align_up(HEADER_SIZE + payload.len(), RECORD_ALIGN);
        if record_len > self.segment_size - SEG_DATA_OFFSET {
            return Err(Error::PayloadTooLarge);
        }

        self.ensure_capacity(record_len)?;

        if (self.write_offset as usize) + record_len > self.segment_size {
            self.roll_segment()?;
        }

        let checksum = MessageHeader::crc32(payload);
        let header = MessageHeader::new_uncommitted(
            self.seq,
            timestamp_ns,
            type_id,
            0,
            checksum,
        );
        let header_bytes = header.to_bytes();
        let offset = self.write_offset as usize;
        self.mmap
            .range_mut(offset, HEADER_SIZE)?
            .copy_from_slice(&header_bytes);
        if !payload.is_empty() {
            self.mmap
                .range_mut(offset + HEADER_SIZE, payload.len())?
                .copy_from_slice(payload);
        }

        let commit_len = MessageHeader::commit_len_for_payload(payload.len())?;
        let header_ptr = unsafe { self.mmap.as_mut_slice().as_mut_ptr().add(offset) };
        MessageHeader::store_commit_len(header_ptr, commit_len);

        self.seq = self.seq.wrapping_add(1);
        self.write_offset = self
            .write_offset
            .checked_add(record_len as u64)
            .ok_or(Error::Corrupt("write offset overflow"))?;
        self.control.set_write_offset(self.write_offset);
        self.control.set_writer_heartbeat_ns(now_ns()?);

        self.control
            .notify_seq()
            .fetch_add(1, Ordering::SeqCst);
        
        // Signal Suppression: Only syscall if someone is sleeping.
        if self.control.waiters_pending().load(Ordering::SeqCst) > 0 {
            futex_wake(self.control.notify_seq())?;
        }
        Ok(())
    }

    fn ensure_capacity(&mut self, record_len: usize) -> Result<()> {
        if self.config.max_segments.is_none() && self.config.max_bytes.is_none() {
            return Ok(());
        }

        let deadline = match self.config.backpressure {
            BackpressurePolicy::FailFast => None,
            BackpressurePolicy::Block { timeout, .. } => timeout.map(|t| Instant::now() + t),
        };

        loop {
            if self.has_capacity(record_len)? {
                return Ok(());
            }

            crate::retention::cleanup_segments(
                &self.path,
                self.segment_id as u64,
                self.write_offset,
                self.segment_size as u64,
            )?;

            if self.has_capacity(record_len)? {
                return Ok(());
            }

            match self.config.backpressure {
                BackpressurePolicy::FailFast => return Err(Error::QueueFull),
                BackpressurePolicy::Block {
                    timeout: _,
                    poll_interval,
                } => {
                    if let Some(deadline) = deadline {
                        if Instant::now() >= deadline {
                            return Err(Error::QueueFull);
                        }
                    }
                    std::thread::sleep(poll_interval);
                }
            }
        }
    }

    fn has_capacity(&mut self, record_len: usize) -> Result<bool> {
        let head_segment = self.segment_id as u64;
        let head_offset = self.write_offset;
        let min_pos = self.retention_cache.min_pos(
            &self.path,
            head_segment,
            head_offset,
            self.segment_size as u64,
        )?;
        let (next_segment, next_offset) = if (self.write_offset as usize) + record_len > self.segment_size
        {
            (head_segment + 1, SEG_DATA_OFFSET as u64 + record_len as u64)
        } else {
            (head_segment, head_offset + record_len as u64)
        };
        let head_after = next_segment
            .saturating_mul(self.segment_size as u64)
            .saturating_add(next_offset);

        let bytes_ok = match self.config.max_bytes {
            Some(max) => head_after.saturating_sub(min_pos) <= max,
            None => true,
        };

        let segments_ok = match self.config.max_segments {
            Some(max) => {
                let min_segment = min_pos / self.segment_size as u64;
                let used = next_segment.saturating_sub(min_segment) + 1;
                used <= max
            }
            None => true,
        };

        Ok(bytes_ok && segments_ok)
    }

    pub fn flush_async(&mut self) -> Result<()> {
        self.mmap.flush_async()?;
        Ok(())
    }

    pub fn flush_sync(&mut self) -> Result<()> {
        self.mmap.flush_sync()?;
        let index_path = self.path.join(INDEX_FILE);
        let index = SegmentIndex::new(self.segment_id as u64, self.write_offset);
        store_index(&index_path, &index)?;
        Ok(())
    }

    pub fn async_seal_error_count(&self) -> u64 {
        self.async_sealer
            .as_ref()
            .map_or(0, |sealer| sealer.error_count())
    }

    pub fn prealloc_error_count(&self) -> u64 {
        self.prealloc.error_count()
    }

    pub fn cleanup(&self) -> Result<Vec<u64>> {
        crate::retention::cleanup_segments(
            &self.path,
            self.segment_id as u64,
            self.write_offset,
            self.segment_size as u64,
        )
    }

    fn roll_segment(&mut self) -> Result<()> {
        let next_segment = self.segment_id + 1;
        let new_mmap = if let Some(prepared) = self.acquire_preallocated(next_segment as u64)? {
            let header = read_segment_header(&prepared)?;
            if header.segment_id != next_segment {
                return Err(Error::Corrupt("preallocated segment id mismatch"));
            }
            prepared
        } else {
            open_or_create_segment(&self.path, next_segment as u64, self.segment_size)?
        };
        if self.config.memlock {
            new_mmap.lock()?;
        }

        self.control
            .set_segment_index(next_segment, SEG_DATA_OFFSET as u64);

        seal_segment(&mut self.mmap)?;
        if self.config.defer_seal_sync {
            let old_mmap = std::mem::replace(&mut self.mmap, new_mmap);
            if let Some(sealer) = &self.async_sealer {
                sealer.submit(old_mmap)?;
            } else {
                old_mmap.sync()?;
            }
        } else {
            self.mmap.sync()?;
            self.mmap = new_mmap;
        }

        self.segment_id = next_segment;
        self.write_offset = SEG_DATA_OFFSET as u64;
        self.control.set_writer_heartbeat_ns(now_ns()?);

        self.control
            .notify_seq()
            .fetch_add(1, Ordering::SeqCst);
        
        // Signal Suppression: Only syscall if someone is sleeping.
        if self.control.waiters_pending().load(Ordering::SeqCst) > 0 {
            futex_wake(self.control.notify_seq())?;
        }
        self.trigger_preallocation(next_segment as u64 + 1);
        Ok(())
    }
}

struct WriterLock {
    _file: File,
}

impl WriterLock {
    fn acquire(path: &Path, writer_epoch: u64) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)?;
        loop {
            if writer_lock::try_lock(&file)? {
                writer_lock::write_lock_record(&file, writer_epoch)?;
                return Ok(Self { _file: file });
            }
            if !writer_lock::writer_alive(path)? {
                continue;
            }
            return Err(Error::WriterAlreadyActive);
        }
    }

    fn update_epoch(&self, writer_epoch: u64) -> Result<()> {
        writer_lock::write_lock_record(&self._file, writer_epoch)
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

fn scan_segment_tail(
    mmap: &MmapFile,
    start_offset: usize,
    segment_size: usize,
) -> Result<(usize, bool)> {
    let mut offset = start_offset.max(SEG_DATA_OFFSET);
    let mut partial = false;

    loop {
        if offset + HEADER_SIZE > segment_size {
            break;
        }
        let commit = MessageHeader::load_commit_len(&mmap.as_slice()[offset] as *const u8);
        if commit == 0 {
            let header_bytes = &mmap.as_slice()[offset..offset + HEADER_SIZE];
            if header_bytes.iter().any(|&b| b != 0) {
                partial = true;
            }
            break;
        }
        let payload_len = match MessageHeader::payload_len_from_commit(commit) {
            Ok(len) => len,
            Err(_) => {
                partial = true;
                break;
            }
        };
        if payload_len > MAX_PAYLOAD_LEN {
            partial = true;
            break;
        }
        let record_len = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN);
        if offset + record_len > segment_size {
            partial = true;
            break;
        }
        offset += record_len;
    }

    Ok((offset, partial))
}

fn repair_tail_and_roll(
    mmap: &mut MmapFile,
    root: &Path,
    segment_id: u32,
    segment_size: usize,
) -> Result<Option<u32>> {
    let header = read_segment_header(mmap)?;
    if (header.flags & crate::segment::SEG_FLAG_SEALED) != 0 {
        let next_segment = segment_id + 1;
        create_segment(root, next_segment as u64, segment_size)?;
        return Ok(Some(next_segment));
    }

    crate::segment::repair_unsealed_tail(mmap, segment_size)?;
    let next_segment = segment_id + 1;
    create_segment(root, next_segment as u64, segment_size)?;
    Ok(Some(next_segment))
}


#[cfg(test)]
mod tests {
    use super::Queue;
    use crate::header::HEADER_SIZE;
    use crate::segment::SEG_DATA_OFFSET;
    use crate::segment::SEG_FLAG_SEALED;
    use crate::segment::create_segment;
    use crate::segment::open_segment;
    use crate::segment::read_segment_header;
    use crate::segment::segment_path;
    use crate::writer::WriterConfig;
    use crate::Error;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn payload_size_accounts_for_segment_header() {
        let dir = tempdir().expect("tempdir");
        let segment_size = 1 * 1024 * 1024;
        let mut writer = Queue::open_publisher_with_config(
            dir.path(),
            WriterConfig {
                segment_size_bytes: segment_size as u64,
                ..WriterConfig::default()
            },
        )
        .expect("open publisher");
        let max_record_len = segment_size - SEG_DATA_OFFSET;
        let max_payload_len = max_record_len - HEADER_SIZE;
        let payload = vec![0u8; max_payload_len];

        writer
            .append_with_timestamp(1, &payload, 0)
            .expect("append max payload");

        let oversized = vec![0u8; max_payload_len + 1];
        let err = writer
            .append_with_timestamp(1, &oversized, 0)
            .expect_err("oversized payload should fail");
        assert!(matches!(err, Error::PayloadTooLarge));
    }

    #[test]
    fn stale_prealloc_is_ignored() {
        let dir = tempdir().expect("tempdir");
        let config = WriterConfig {
            segment_size_bytes: 4096,
            ..WriterConfig::default()
        };
        let mut writer = Queue::open_publisher_with_config(dir.path(), config)
            .expect("open publisher");

        let current = writer.segment_id as u64;
        let stale_id = current + 2;
        writer.trigger_preallocation(stale_id);
        let stale_path = segment_path(dir.path(), stale_id);
        for _ in 0..100 {
            if stale_path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let _ = writer.acquire_preallocated(current + 1).expect("acquire");

        writer.roll_segment().expect("roll segment");
        assert_eq!(writer.segment_id as u64, current + 1);
    }

    #[test]
    fn recovery_advances_on_control_segment_even_if_unsealed() {
        let dir = tempdir().expect("tempdir");
        let config = WriterConfig {
            segment_size_bytes: 4096,
            ..WriterConfig::default()
        };
        let mut writer =
            Queue::open_publisher_with_config(dir.path(), config).expect("open publisher");

        writer
            .append_with_timestamp(1, b"alpha", 0)
            .expect("append alpha");

        let current_segment = writer.segment_id;
        let next_segment = current_segment + 1;
        create_segment(dir.path(), next_segment as u64, writer.segment_size)
            .expect("create next segment");
        writer
            .control
            .set_segment_index(next_segment, SEG_DATA_OFFSET as u64);
        drop(writer);

        let writer = Queue::open_publisher_with_config(dir.path(), config)
            .expect("reopen publisher");
        assert_eq!(writer.segment_id, next_segment);

        let old_mmap = open_segment(dir.path(), current_segment as u64, writer.segment_size)
            .expect("open old segment");
        let old_header = read_segment_header(&old_mmap).expect("read old header");
        assert_eq!(old_header.flags & SEG_FLAG_SEALED, SEG_FLAG_SEALED);
    }
}
