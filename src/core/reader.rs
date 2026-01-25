use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::core::control::ControlFile;
use crate::core::header::{MessageHeader, HEADER_SIZE, MAX_PAYLOAD_LEN, PAD_TYPE_ID, RECORD_ALIGN};
use crate::core::mmap::MmapFile;
use crate::core::seek_index::{
    load_index_entries, load_index_header, SeekIndexEntry, SeekIndexHeader,
};
use crate::core::segment::{
    load_reader_meta, open_segment, read_segment_header, segment_path, store_reader_meta,
    validate_segment_size, ReaderMeta, SEG_DATA_OFFSET, SEG_FLAG_SEALED,
};
use crate::core::wait::futex_wait;
use crate::core::writer::Queue;
use crate::core::writer_lock;
use crate::core::{Error, Result};

const READERS_DIR: &str = "readers";
const WRITER_LOCK_FILE: &str = "writer.lock";
const DEFAULT_SPIN_US: u32 = 10;
const WRITER_TTL_NS: u64 = 5_000_000_000;

pub struct MessageView<'a> {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub type_id: u16,
    pub payload: &'a [u8],
}

pub(crate) struct MessageRef {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub type_id: u16,
    pub payload_offset: usize,
    pub payload_len: usize,
}

pub enum WaitStrategy {
    /// True busy-spinning. Burns 100% CPU on a single core for maximum responsiveness.
    BusySpin,
    /// High-performance hybrid: spins for a period, then parks (sleeps) in the kernel.
    SpinThenPark { spin_us: u32 },
    /// Low-priority periodic polling: yields to the OS for a fixed duration.
    Sleep(Duration),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartMode {
    /// Resume from the last saved position. If the segment is missing (retention),
    /// fail with an error.
    ResumeStrict,
    /// Resume from the last saved position. If the segment is missing,
    /// snap to the oldest available segment.
    ResumeSnapshot,
    /// Resume from the last saved position. If the segment is missing,
    /// snap to the current writer position (latest).
    ResumeLatest,
    /// Ignore previous state and start at the current writer position.
    Latest,
    /// Ignore previous state and start at the oldest available segment.
    Earliest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisconnectReason {
    WriterLockLost,
    HeartbeatStale,
    SegmentMissing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriterStatus {
    pub alive: bool,
    pub last_heartbeat_ns: u64,
    pub ttl_ns: u64,
}

impl Default for StartMode {
    fn default() -> Self {
        Self::ResumeStrict
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ReaderConfig {
    pub memlock: bool,
    pub start_mode: StartMode,
}

impl Default for ReaderConfig {
    fn default() -> Self {
        Self {
            memlock: false,
            start_mode: StartMode::default(),
        }
    }
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
    segment_size: usize,
    memlock: bool,
}

impl Queue {
    pub fn open_subscriber(path: impl AsRef<std::path::Path>, reader: &str) -> Result<QueueReader> {
        Self::open_subscriber_with_config(path, reader, ReaderConfig::default())
    }

    pub fn try_open_subscriber(
        path: impl AsRef<std::path::Path>,
        reader: &str,
    ) -> Result<Option<QueueReader>> {
        Self::try_open_subscriber_with_config(path, reader, ReaderConfig::default())
    }

    pub fn open_subscriber_with_config(
        path: impl AsRef<std::path::Path>,
        reader: &str,
        config: ReaderConfig,
    ) -> Result<QueueReader> {
        let path = path.as_ref().to_path_buf();
        let control_path = path.join("control.meta");
        let control = ControlFile::open(&control_path)?;
        control.wait_ready()?;
        Self::build_reader(path, reader, control, config)
    }

    pub fn try_open_subscriber_with_config(
        path: impl AsRef<std::path::Path>,
        reader: &str,
        config: ReaderConfig,
    ) -> Result<Option<QueueReader>> {
        let path = path.as_ref().to_path_buf();
        let control_path = path.join("control.meta");
        let control = match ControlFile::open(&control_path) {
            Ok(control) => control,
            Err(Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        if !control.check_ready()? {
            return Ok(None);
        }
        let reader = Self::build_reader(path, reader, control, config)?;
        Ok(Some(reader))
    }

    fn build_reader(
        path: PathBuf,
        reader: &str,
        control: ControlFile,
        config: ReaderConfig,
    ) -> Result<QueueReader> {
        if reader.is_empty() {
            return Err(Error::Unsupported("reader name cannot be empty"));
        }
        let segment_size = validate_segment_size(control.segment_size())?;

        let readers_dir = path.join(READERS_DIR);
        std::fs::create_dir_all(&readers_dir)?;
        let meta_path = readers_dir.join(format!("{reader}.meta"));
        let mut meta = load_reader_meta(&meta_path)?;

        let segment_path_check = segment_path(&path, meta.segment_id);
        let should_reset = match config.start_mode {
            StartMode::ResumeStrict => false,
            StartMode::ResumeSnapshot | StartMode::ResumeLatest => !segment_path_check.exists(),
            StartMode::Latest | StartMode::Earliest => true,
        };

        if should_reset {
            match config.start_mode {
                StartMode::Latest | StartMode::ResumeLatest => {
                    let (seg, off) = control.segment_index();
                    meta.segment_id = seg as u64;
                    meta.offset = off;
                }
                StartMode::Earliest | StartMode::ResumeSnapshot => {
                    match find_oldest_segment(&path) {
                        Ok(id) => {
                            meta.segment_id = id;
                            meta.offset = SEG_DATA_OFFSET as u64;
                        }
                        Err(_) => {
                            let (seg, _) = control.segment_index();
                            meta.segment_id = seg as u64;
                            meta.offset = SEG_DATA_OFFSET as u64;
                        }
                    }
                }
                _ => {}
            }
        }

        if meta.offset < SEG_DATA_OFFSET as u64 {
            meta.offset = SEG_DATA_OFFSET as u64;
        }
        meta.last_heartbeat_ns = now_ns()?;
        store_reader_meta(&meta_path, &mut meta)?;

        let segment_path = segment_path(&path, meta.segment_id);
        if !segment_path.exists() {
            return Err(Error::Corrupt("reader segment missing"));
        }
        let mmap = open_segment(&path, meta.segment_id, segment_size)?;
        if config.memlock {
            control.lock()?;
            mmap.lock()?;
        }

        Ok(QueueReader {
            path,
            control,
            mmap,
            segment_id: meta.segment_id as u32,
            read_offset: meta.offset,
            meta_path,
            meta,
            wait_strategy: WaitStrategy::SpinThenPark {
                spin_us: DEFAULT_SPIN_US,
            },
            segment_size,
            memlock: config.memlock,
        })
    }
}

fn find_oldest_segment(path: &std::path::Path) -> Result<u64> {
    let mut min_id = None;
    for entry in std::fs::read_dir(path).map_err(Error::Io)? {
        let entry = entry.map_err(Error::Io)?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".q") && name_str.len() == 11 {
            if let Ok(id) = name_str[0..9].parse::<u64>() {
                min_id = Some(match min_id {
                    Some(val) => std::cmp::min(val, id),
                    None => id,
                });
            }
        }
    }
    min_id.ok_or(Error::Corrupt("no segments found"))
}

impl QueueReader {
    pub fn seek_seq(&mut self, target_seq: u64) -> Result<bool> {
        let segments = list_segments(&self.path)?;
        if segments.is_empty() {
            return Ok(false);
        }
        let headers = load_seek_headers(&self.path, &segments)?;
        let header = select_header_for_seq(&headers, target_seq);
        let segment_id = header.map(|h| h.segment_id).unwrap_or(segments[0]);
        let mut offset = SEG_DATA_OFFSET as u64;
        if let Some(header) = header {
            offset = header.data_offset as u64;
            let entries = load_index_entries(&self.path, header)?;
            if let Some(entry) = find_entry_by_seq(&entries, target_seq) {
                offset = entry.offset.max(SEG_DATA_OFFSET as u64);
            }
        }
        self.open_segment_for_seek(segment_id, offset)?;
        self.scan_to_seq(target_seq)
    }

    pub fn seek_timestamp(&mut self, target_ts_ns: u64) -> Result<bool> {
        let segments = list_segments(&self.path)?;
        if segments.is_empty() {
            return Ok(false);
        }
        let headers = load_seek_headers(&self.path, &segments)?;
        let header = select_header_for_timestamp(&headers, target_ts_ns);
        let segment_id = header.map(|h| h.segment_id).unwrap_or(segments[0]);
        let mut offset = SEG_DATA_OFFSET as u64;
        if let Some(header) = header {
            offset = header.data_offset as u64;
            let entries = load_index_entries(&self.path, header)?;
            if let Some(entry) = find_entry_by_timestamp(&entries, target_ts_ns) {
                offset = entry.offset.max(SEG_DATA_OFFSET as u64);
            }
        }
        self.open_segment_for_seek(segment_id, offset)?;
        self.scan_to_timestamp(target_ts_ns)
    }

    pub fn next(&mut self) -> Result<Option<MessageView<'_>>> {
        let Some(message) = self.next_ref()? else {
            return Ok(None);
        };
        let payload = self.payload_at(message.payload_offset, message.payload_len)?;
        Ok(Some(MessageView {
            seq: message.seq,
            timestamp_ns: message.timestamp_ns,
            type_id: message.type_id,
            payload,
        }))
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
            WaitStrategy::BusySpin => {
                let deadline = timeout.map(|t| std::time::Instant::now() + t);
                loop {
                    if self.peek_committed()? {
                        return Ok(());
                    }
                    if let Some(d) = deadline {
                        if std::time::Instant::now() >= d {
                            return Ok(());
                        }
                    }
                    std::hint::spin_loop();
                }
            }
            WaitStrategy::Sleep(duration) => {
                std::thread::sleep(duration);
                return Ok(());
            }
            WaitStrategy::SpinThenPark { spin_us } => {
                let spin_deadline =
                    std::time::Instant::now() + Duration::from_micros(spin_us as u64);
                let mut i = 0;
                loop {
                    if self.peek_committed()? {
                        return Ok(());
                    }
                    i += 1;
                    if i % 128 == 0 && std::time::Instant::now() >= spin_deadline {
                        break;
                    }
                    // Hint to the CPU that we are in a spin-wait loop.
                    // This improves performance on hyper-threaded cores by yielding execution resources
                    // to the other hardware thread.
                    std::hint::spin_loop();
                }
            }
        }

        // Signal Suppression Protocol:
        // 1. Register presence (SeqCst to ensure visibility before check)
        self.control
            .waiters_pending()
            .fetch_add(1, Ordering::SeqCst);

        // 2. Load seq before the check-after-set to avoid missing a wake.
        let seq = self.control.notify_seq().load(Ordering::Acquire);

        // 3. Double-check for data (Check-After-Set).
        // This handles the race where Writer wrote *just* before we incremented.
        if self.peek_committed()? {
            self.control
                .waiters_pending()
                .fetch_sub(1, Ordering::SeqCst);
            return Ok(());
        }

        // 4. Sleep
        // Note: futex_wait internally handles the race if seq changes between load and syscall.
        let res = futex_wait(self.control.notify_seq(), seq, timeout);

        // 5. Deregister
        self.control
            .waiters_pending()
            .fetch_sub(1, Ordering::SeqCst);

        res
    }

    pub fn set_wait_strategy(&mut self, strategy: WaitStrategy) {
        self.wait_strategy = strategy;
    }

    pub fn segment_id(&self) -> u32 {
        self.segment_id
    }

    pub fn writer_status(&self, ttl: Duration) -> Result<WriterStatus> {
        let lock_path = self.path.join(WRITER_LOCK_FILE);
        let lock_alive = writer_lock::writer_alive(&lock_path)?;
        let heartbeat = self.control.writer_heartbeat_ns();
        let ttl_ns = ttl_ns(ttl);
        let now = now_ns()?;
        let heartbeat_stale = heartbeat != 0 && now.saturating_sub(heartbeat) > ttl_ns;
        let alive = lock_alive || (heartbeat != 0 && !heartbeat_stale);
        Ok(WriterStatus {
            alive,
            last_heartbeat_ns: heartbeat,
            ttl_ns,
        })
    }

    pub fn detect_disconnect(&self, ttl: Duration) -> Result<Option<DisconnectReason>> {
        let segment_path = segment_path(&self.path, self.segment_id as u64);
        if !segment_path.exists() {
            return Ok(Some(DisconnectReason::SegmentMissing));
        }
        self.writer_dead_reason(ttl_ns(ttl))
    }

    pub(crate) fn next_ref(&mut self) -> Result<Option<MessageRef>> {
        let last_possible = (self.segment_size - HEADER_SIZE) as u64;
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
                if self.advance_segment()? {
                    continue;
                }
                return Ok(None);
            }

            let payload_len = MessageHeader::payload_len_from_commit(commit)?;
            if payload_len > MAX_PAYLOAD_LEN {
                return Err(Error::Corrupt("payload length exceeds max"));
            }
            let record_len = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN);
            if offset + record_len > self.segment_size {
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
            let payload = self.payload_at(payload_start, payload_len)?;

            if header.type_id == PAD_TYPE_ID {
                continue;
            }
            header.validate_crc(payload)?;
            return Ok(Some(MessageRef {
                seq: header.seq,
                timestamp_ns: header.timestamp_ns,
                type_id: header.type_id,
                payload_offset: payload_start,
                payload_len,
            }));
        }
    }

    fn maybe_heartbeat(&mut self) -> Result<()> {
        let now = now_ns()?;
        if now.saturating_sub(self.meta.last_heartbeat_ns) > 1_000_000_000 {
            self.meta.last_heartbeat_ns = now;
            store_reader_meta(&self.meta_path, &mut self.meta)?;
        }
        Ok(())
    }

    pub(crate) fn peek_committed(&self) -> Result<bool> {
        let last_possible = self.segment_size - HEADER_SIZE;
        if self.read_offset as usize > last_possible {
            return Ok(false);
        }
        let offset = self.read_offset as usize;
        let commit = MessageHeader::load_commit_len(&self.mmap.as_slice()[offset] as *const u8);
        Ok(commit > 0)
    }

    pub(crate) fn payload_at(&self, offset: usize, len: usize) -> Result<&[u8]> {
        let end = offset
            .checked_add(len)
            .ok_or(Error::Corrupt("payload offset overflow"))?;
        self.mmap
            .as_slice()
            .get(offset..end)
            .ok_or(Error::Corrupt("payload out of bounds"))
    }

    fn advance_segment(&mut self) -> Result<bool> {
        // Optimization: Check the Control Block first.
        // If the writer hasn't incremented current_segment, the next file likely doesn't exist yet.
        // This avoids a `stat` syscall loop while waiting for a rollover.
        if self.control.current_segment() <= self.segment_id {
            return Ok(false);
        }

        let header = read_segment_header(&self.mmap)?;
        let next_segment = self.segment_id + 1;
        let next_path = segment_path(&self.path, next_segment as u64);

        // We still check exists() or try_open because current_segment is a "hint" (Relaxed).
        // However, we only pay this cost if the hint suggests we should.
        if !next_path.exists() {
            return Ok(false);
        }
        if (header.flags & SEG_FLAG_SEALED) == 0 {
            if !self.writer_dead()? {
                return Ok(false);
            }
            crate::core::segment::repair_unsealed_tail(&mut self.mmap, self.segment_size)?;
        }
        let mmap = open_segment(&self.path, next_segment as u64, self.segment_size)?;
        if self.memlock {
            mmap.lock()?;
        }
        self.mmap = mmap;
        self.segment_id = next_segment;
        self.read_offset = SEG_DATA_OFFSET as u64;
        Ok(true)
    }

    fn writer_dead(&self) -> Result<bool> {
        Ok(self.writer_dead_reason(WRITER_TTL_NS)?.is_some())
    }

    fn writer_dead_reason(&self, ttl_ns: u64) -> Result<Option<DisconnectReason>> {
        let lock_path = self.path.join(WRITER_LOCK_FILE);
        let lock_alive = writer_lock::writer_alive(&lock_path)?;
        let heartbeat = self.control.writer_heartbeat_ns();
        if lock_alive {
            if heartbeat != 0 {
                let now = now_ns()?;
                if now.saturating_sub(heartbeat) > ttl_ns {
                    return Ok(Some(DisconnectReason::HeartbeatStale));
                }
            }
            return Ok(None);
        }

        if heartbeat == 0 {
            return Ok(Some(DisconnectReason::WriterLockLost));
        }
        let now = now_ns()?;
        if now.saturating_sub(heartbeat) > ttl_ns {
            return Ok(Some(DisconnectReason::WriterLockLost));
        }
        Ok(None)
    }

    fn open_segment_for_seek(&mut self, segment_id: u64, offset: u64) -> Result<()> {
        let mut resolved_segment = segment_id;
        let mmap = match open_segment(&self.path, segment_id, self.segment_size) {
            Ok(mmap) => mmap,
            Err(Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                let segments = list_segments(&self.path)?;
                if segments.is_empty() {
                    return Err(Error::Corrupt("no segments found"));
                }
                resolved_segment = segments[0];
                open_segment(&self.path, resolved_segment, self.segment_size)?
            }
            Err(err) => return Err(err),
        };
        if self.memlock {
            mmap.lock()?;
        }
        self.mmap = mmap;
        self.segment_id = resolved_segment as u32;
        if resolved_segment == segment_id {
            self.read_offset = offset.max(SEG_DATA_OFFSET as u64);
        } else {
            self.read_offset = SEG_DATA_OFFSET as u64;
        }
        self.meta.segment_id = self.segment_id as u64;
        self.meta.offset = self.read_offset;
        Ok(())
    }

    fn scan_to_seq(&mut self, target_seq: u64) -> Result<bool> {
        loop {
            let Some(message) = self.next_ref()? else {
                return Ok(false);
            };
            if message.seq >= target_seq {
                let record_start = message.payload_offset.saturating_sub(HEADER_SIZE) as u64;
                self.read_offset = record_start;
                self.meta.segment_id = self.segment_id as u64;
                self.meta.offset = self.read_offset;
                return Ok(true);
            }
        }
    }

    fn scan_to_timestamp(&mut self, target_ts_ns: u64) -> Result<bool> {
        loop {
            let Some(message) = self.next_ref()? else {
                return Ok(false);
            };
            if message.timestamp_ns >= target_ts_ns {
                let record_start = message.payload_offset.saturating_sub(HEADER_SIZE) as u64;
                self.read_offset = record_start;
                self.meta.segment_id = self.segment_id as u64;
                self.meta.offset = self.read_offset;
                return Ok(true);
            }
        }
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

fn list_segments(path: &std::path::Path) -> Result<Vec<u64>> {
    let mut segments = Vec::new();
    for entry in std::fs::read_dir(path).map_err(Error::Io)? {
        let entry = entry.map_err(Error::Io)?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".q") && name_str.len() == 11 {
            if let Ok(id) = name_str[0..9].parse::<u64>() {
                segments.push(id);
            }
        }
    }
    segments.sort_unstable();
    Ok(segments)
}

fn load_seek_headers(path: &std::path::Path, segments: &[u64]) -> Result<Vec<SeekIndexHeader>> {
    let mut headers = Vec::new();
    for &segment_id in segments {
        if let Some(header) = load_index_header(path, segment_id)? {
            headers.push(header);
        }
    }
    Ok(headers)
}

fn select_header_for_seq<'a>(
    headers: &'a [SeekIndexHeader],
    target_seq: u64,
) -> Option<&'a SeekIndexHeader> {
    if headers.is_empty() {
        return None;
    }
    let first = &headers[0];
    if target_seq <= first.min_seq {
        return Some(first);
    }
    let last = headers.last()?;
    if target_seq > last.max_seq {
        return Some(last);
    }
    for header in headers {
        if target_seq >= header.min_seq && target_seq <= header.max_seq {
            return Some(header);
        }
    }
    for header in headers {
        if target_seq <= header.max_seq {
            return Some(header);
        }
    }
    Some(last)
}

fn select_header_for_timestamp<'a>(
    headers: &'a [SeekIndexHeader],
    target_ts_ns: u64,
) -> Option<&'a SeekIndexHeader> {
    if headers.is_empty() {
        return None;
    }
    let first = &headers[0];
    if target_ts_ns <= first.min_ts_ns {
        return Some(first);
    }
    let last = headers.last()?;
    if target_ts_ns > last.max_ts_ns {
        return Some(last);
    }
    for header in headers {
        if target_ts_ns >= header.min_ts_ns && target_ts_ns <= header.max_ts_ns {
            return Some(header);
        }
    }
    for header in headers {
        if target_ts_ns <= header.max_ts_ns {
            return Some(header);
        }
    }
    Some(last)
}

fn find_entry_by_seq<'a>(
    entries: &'a [SeekIndexEntry],
    target_seq: u64,
) -> Option<&'a SeekIndexEntry> {
    if entries.is_empty() {
        return None;
    }
    let mut lo = 0usize;
    let mut hi = entries.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if entries[mid].seq <= target_seq {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo == 0 {
        None
    } else {
        entries.get(lo - 1)
    }
}

fn find_entry_by_timestamp<'a>(
    entries: &'a [SeekIndexEntry],
    target_ts_ns: u64,
) -> Option<&'a SeekIndexEntry> {
    let mut best = None;
    for entry in entries {
        if entry.timestamp_ns <= target_ts_ns {
            best = Some(entry);
        }
    }
    best
}

fn ttl_ns(ttl: Duration) -> u64 {
    u64::try_from(ttl.as_nanos()).unwrap_or(u64::MAX)
}
