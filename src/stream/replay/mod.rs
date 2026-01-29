use crate::core::{Queue, QueueReader, ReaderConfig, WaitStrategy};
use crate::protocol::{
    BookEventHeader, BookEventType, BookMode, L2Diff, L2Snapshot, PriceLevelUpdate, TypeId,
};
use crate::stream::{StreamMessageRef, StreamReader};
use crate::core::sequencer::{GapDetection, SequenceValidator};
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub mod snapshot;
mod transform;
mod sink;
mod pipeline;
mod parallel;

pub use transform::{Transform, Identity, TimeRangeFilter, TypeFilter, Sampler, TransformChain};
pub use sink::{Sink, QueueSink, NullSink, VecSink};
#[cfg(feature = "stream")]
pub use sink::CsvSink;
pub use pipeline::{ReplayPipeline, ReplayStats};
pub use parallel::{MultiSymbolReplay, SymbolHandler, WorkerStats, MultiReplayStats};
pub use crate::core::sequencer::GapPolicy;

pub struct ReplayEngine<R: StreamReader> {
    root: PathBuf,
    reader: R,
    policy: ReplayPolicy,
    l2_book: L2Book,
    sequencer: SequenceValidator,
    last_gap: Option<(u64, u64)>,
}

pub type LiveReplayEngine = ReplayEngine<QueueReaderAdapter>;

/// Adapter to make QueueReader work with StreamReader trait
pub struct QueueReaderAdapter {
    reader: QueueReader,
}

impl QueueReaderAdapter {
    pub fn open(path: impl AsRef<Path>, reader_name: &str) -> Result<Self> {
        let reader = Queue::open_subscriber(path.as_ref(), reader_name)?;
        Ok(Self { reader })
    }

    pub fn open_with_config(
        path: impl AsRef<Path>,
        reader_name: &str,
        config: ReaderConfig,
    ) -> Result<Self> {
        let reader = Queue::open_subscriber_with_config(path.as_ref(), reader_name, config)?;
        Ok(Self { reader })
    }
}

impl StreamReader for QueueReaderAdapter {
    fn next<'a>(&'a mut self) -> Result<Option<StreamMessageRef<'a>>> {
        let msg = self.reader.next()?;
        Ok(msg.map(StreamMessageRef::from))
    }

    fn wait(&mut self, timeout: Option<std::time::Duration>) -> Result<()> {
        Ok(self.reader.wait(timeout)?)
    }

    fn commit(&mut self) -> Result<()> {
        Ok(self.reader.commit()?)
    }

    fn set_wait_strategy(&mut self, strategy: WaitStrategy) {
        self.reader.set_wait_strategy(strategy);
    }

    fn seek_seq(&mut self, seq: u64) -> Result<bool> {
        Ok(self.reader.seek_seq(seq)?)
    }

    fn seek_timestamp(&mut self, target_ts_ns: u64) -> Result<bool> {
        Ok(self.reader.seek_timestamp(target_ts_ns)?)
    }
}

// #[cfg(feature = "storage")]
// pub type ArchiveReplayEngine = ReplayEngine<crate::stream::archive::ArchiveStream>; // REMOVED: storage module deleted

impl ReplayEngine<QueueReaderAdapter> {
    pub fn open(path: impl AsRef<Path>, reader: &str) -> Result<Self> {
        let root = path.as_ref().to_path_buf();
        let reader = QueueReaderAdapter::open(&root, reader)?;
        Ok(Self::new(root, reader))
    }

    pub fn open_with_config(
        path: impl AsRef<Path>,
        reader: &str,
        config: ReaderConfig,
    ) -> Result<Self> {
        let root = path.as_ref().to_path_buf();
        let reader = QueueReaderAdapter::open_with_config(&root, reader, config)?;
        Ok(Self::new(root, reader))
    }
}

impl<R: StreamReader> ReplayEngine<R> {
    pub fn new(root: impl AsRef<Path>, reader: R) -> Self {
        let policy = ReplayPolicy::default();
        Self {
            root: root.as_ref().to_path_buf(),
            reader,
            policy,
            l2_book: L2Book::new(),
            sequencer: SequenceValidator::new(policy.gap),
            last_gap: None,
        }
    }

    pub fn set_wait_strategy(&mut self, strategy: WaitStrategy) {
        self.reader.set_wait_strategy(strategy);
    }

    pub fn policy_mut(&mut self) -> &mut ReplayPolicy {
        &mut self.policy
    }

    pub fn set_policy(&mut self, policy: ReplayPolicy) {
        self.policy = policy;
        self.sequencer.set_policy(policy.gap);
    }

    pub fn last_seq(&self) -> Option<u64> {
        self.sequencer.last_seq()
    }

    pub fn l2_book(&self) -> &L2Book {
        &self.l2_book
    }

    pub fn reader_mut(&mut self) -> &mut R {
        &mut self.reader
    }

    pub fn last_gap(&self) -> Option<(u64, u64)> {
        self.last_gap
    }

    pub fn next(&mut self) -> Result<Option<ReplayUpdate>> {
        let Some(msg) = self.reader.next()? else {
            return Ok(None);
        };

        let applied = apply_message(
            &mut self.sequencer,
            &mut self.last_gap,
            &mut self.l2_book,
            msg,
        )?;
        Ok(Some(applied.update))
    }

    pub fn next_message(&mut self) -> Result<Option<ReplayMessage<'_>>> {
        let Some(msg) = self.reader.next()? else {
            return Ok(None);
        };

        let applied = apply_message(
            &mut self.sequencer,
            &mut self.last_gap,
            &mut self.l2_book,
            msg,
        )?;

        Ok(Some(ReplayMessage {
            msg,
            update: applied.update,
            book_event: applied.book_event,
            book: &self.l2_book,
        }))
    }

    pub fn wait(&mut self, timeout: Option<std::time::Duration>) -> Result<()> {
        self.reader.wait(timeout).map_err(|e| e.into())
    }

    pub fn commit(&mut self) -> Result<()> {
        self.reader.commit().map_err(|e| e.into())
    }

    pub fn warm_start_from_snapshot_path(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<snapshot::SnapshotHeader> {
        let (header, payload) = snapshot::load_snapshot(path.as_ref())?;
        self.apply_snapshot_payload(&header, &payload)?;
        // Update sequencer to expect next sequence after snapshot
        self.sequencer.reset();
        self.sequencer.check(header.seq_num)?;
        self.last_gap = None;
        let _ = self.reader.seek_seq(header.seq_num.saturating_add(1))?;
        Ok(header)
    }

    pub fn warm_start_latest_snapshot(
        &mut self,
        venue_id: u32,
        market_id: u32,
    ) -> Result<Option<snapshot::SnapshotHeader>> {
        let dir = snapshot::snapshot_dir(&self.root, venue_id, market_id);
        let Some(info) = snapshot::latest_snapshot_before_seq(&dir, u64::MAX)? else {
            return Ok(None);
        };
        let header = self.warm_start_from_snapshot_path(&info.path)?;
        Ok(Some(header))
    }

    pub fn warm_start_to_exchange_ts(
        &mut self,
        venue_id: u32,
        market_id: u32,
        target_ts_ns: u64,
    ) -> Result<bool> {
        let dir = snapshot::snapshot_dir(&self.root, venue_id, market_id);
        let Some(info) = snapshot::latest_snapshot_before_exchange_ts(&dir, target_ts_ns)? else {
            return Ok(false);
        };
        let header = self.warm_start_from_snapshot_path(&info.path)?;
        if header.venue_id != venue_id || header.market_id != market_id {
            bail!("snapshot venue/market mismatch");
        }
        self.replay_until_exchange_ts(target_ts_ns)
    }

    pub fn warm_start_to_ingest_ts(
        &mut self,
        venue_id: u32,
        market_id: u32,
        target_ts_ns: u64,
    ) -> Result<bool> {
        let dir = snapshot::snapshot_dir(&self.root, venue_id, market_id);
        let Some(info) = snapshot::latest_snapshot_before_ingest_ts(&dir, target_ts_ns)? else {
            return Ok(false);
        };
        let header = self.warm_start_from_snapshot_path(&info.path)?;
        if header.venue_id != venue_id || header.market_id != market_id {
            bail!("snapshot venue/market mismatch");
        }
        self.replay_until_ingest_ts(target_ts_ns)
    }
}

struct AppliedMessage<'a> {
    update: ReplayUpdate,
    book_event: Option<BookEvent<'a>>,
}

impl<R: StreamReader> ReplayEngine<R> {
    fn apply_snapshot_payload(
        &mut self,
        header: &snapshot::SnapshotHeader,
        payload: &[u8],
    ) -> Result<()> {
        if header.schema_version != snapshot::SNAPSHOT_VERSION {
            bail!(
                "unsupported snapshot schema version {}",
                header.schema_version
            );
        }
        if header.book_mode != BookMode::L2 as u16 {
            bail!("unsupported snapshot book mode {}", header.book_mode);
        }
        let Some(snapshot) = read_copy::<L2Snapshot>(payload, 0) else {
            bail!("snapshot payload too small for L2Snapshot");
        };
        let levels_offset = std::mem::size_of::<L2Snapshot>();
        let Some(levels) = LevelsView::new(
            payload,
            levels_offset,
            snapshot.bid_count as usize,
            snapshot.ask_count as usize,
        ) else {
            bail!("snapshot payload missing price levels");
        };
        self.l2_book
            .apply_snapshot(&snapshot, levels.bids_iter(), levels.asks_iter());
        Ok(())
    }

    fn replay_until_exchange_ts(&mut self, target_ts_ns: u64) -> Result<bool> {
        loop {
            let Some(msg) = self.reader.next()? else {
                return Ok(false);
            };
            let applied = apply_message(
                &mut self.sequencer,
                &mut self.last_gap,
                &mut self.l2_book,
                msg,
            )?;
            if let Some(event) = applied.book_event {
                if event.header.exchange_ts_ns >= target_ts_ns {
                    return Ok(true);
                }
            }
        }
    }

    fn replay_until_ingest_ts(&mut self, target_ts_ns: u64) -> Result<bool> {
        loop {
            let Some(msg) = self.reader.next()? else {
                return Ok(false);
            };
            let applied = apply_message(
                &mut self.sequencer,
                &mut self.last_gap,
                &mut self.l2_book,
                msg,
            )?;
            if let Some(event) = applied.book_event {
                if event.header.ingest_ts_ns >= target_ts_ns {
                    return Ok(true);
                }
            }
        }
    }
}

fn apply_message<'a>(
    sequencer: &mut SequenceValidator,
    last_gap: &mut Option<(u64, u64)>,
    l2_book: &mut L2Book,
    msg: StreamMessageRef<'a>,
) -> Result<AppliedMessage<'a>> {
    // Check sequence with the validator
    match sequencer.check(msg.seq)? {
        GapDetection::Sequential => {
            // Sequence is OK, continue processing
        }
        GapDetection::Gap { from, to } => {
            *last_gap = Some((from, to));
            // Quarantine policy is handled by the validator returning Ok with Gap detection
            if sequencer.policy() == GapPolicy::Quarantine {
                return Ok(AppliedMessage {
                    update: ReplayUpdate::Skipped {
                        seq: msg.seq,
                        reason: SkipReason::Gap,
                    },
                    book_event: None,
                });
            }
            // Ignore policy: validator already updated sequence, just continue
        }
    }

    if msg.type_id != TypeId::BookEvent.as_u16() {
        return Ok(AppliedMessage {
            update: ReplayUpdate::Skipped {
                seq: msg.seq,
                reason: SkipReason::NonBookEvent,
            },
            book_event: None,
        });
    }

    let payload = msg.payload;
    let Some(header) = read_copy::<BookEventHeader>(payload, 0) else {
        return Ok(AppliedMessage {
            update: ReplayUpdate::Skipped {
                seq: msg.seq,
                reason: SkipReason::Corrupt,
            },
            book_event: None,
        });
    };

    if header.book_mode != BookMode::L2 as u8 {
        return Ok(AppliedMessage {
            update: ReplayUpdate::Skipped {
                seq: msg.seq,
                reason: SkipReason::UnsupportedMode,
            },
            book_event: Some(BookEvent {
                header,
                payload: BookEventPayload::Unsupported,
            }),
        });
    }

    let payload_offset = std::mem::size_of::<BookEventHeader>();

    let (update, book_event) = match header.event_type {
        x if x == BookEventType::Snapshot as u8 => {
            let Some(snapshot) = read_copy::<L2Snapshot>(payload, payload_offset) else {
                return Ok(AppliedMessage {
                    update: ReplayUpdate::Skipped {
                        seq: msg.seq,
                        reason: SkipReason::Corrupt,
                    },
                    book_event: Some(BookEvent {
                        header,
                        payload: BookEventPayload::Unsupported,
                    }),
                });
            };
            let levels_offset = payload_offset + std::mem::size_of::<L2Snapshot>();
            let Some(levels) = LevelsView::new(
                payload,
                levels_offset,
                snapshot.bid_count as usize,
                snapshot.ask_count as usize,
            ) else {
                return Ok(AppliedMessage {
                    update: ReplayUpdate::Skipped {
                        seq: msg.seq,
                        reason: SkipReason::Corrupt,
                    },
                    book_event: Some(BookEvent {
                        header,
                        payload: BookEventPayload::Unsupported,
                    }),
                });
            };
            l2_book.apply_snapshot(&snapshot, levels.bids_iter(), levels.asks_iter());
            let book_event = Some(BookEvent {
                header,
                payload: BookEventPayload::Snapshot { snapshot, levels },
            });
            let update = ReplayUpdate::Applied {
                seq: msg.seq,
                event_type: BookEventType::Snapshot,
            };
            (update, book_event)
        }
        x if x == BookEventType::Diff as u8 => {
            let Some(diff) = read_copy::<L2Diff>(payload, payload_offset) else {
                return Ok(AppliedMessage {
                    update: ReplayUpdate::Skipped {
                        seq: msg.seq,
                        reason: SkipReason::Corrupt,
                    },
                    book_event: Some(BookEvent {
                        header,
                        payload: BookEventPayload::Unsupported,
                    }),
                });
            };
            let levels_offset = payload_offset + std::mem::size_of::<L2Diff>();
            let Some(levels) = LevelsView::new(
                payload,
                levels_offset,
                diff.bid_count as usize,
                diff.ask_count as usize,
            ) else {
                return Ok(AppliedMessage {
                    update: ReplayUpdate::Skipped {
                        seq: msg.seq,
                        reason: SkipReason::Corrupt,
                    },
                    book_event: Some(BookEvent {
                        header,
                        payload: BookEventPayload::Unsupported,
                    }),
                });
            };
            l2_book.apply_diff(&diff, levels.bids_iter(), levels.asks_iter());
            let book_event = Some(BookEvent {
                header,
                payload: BookEventPayload::Diff { diff, levels },
            });
            let update = ReplayUpdate::Applied {
                seq: msg.seq,
                event_type: BookEventType::Diff,
            };
            (update, book_event)
        }
        _ => {
            let book_event = Some(BookEvent {
                header,
                payload: BookEventPayload::Unsupported,
            });
            let update = ReplayUpdate::Skipped {
                seq: msg.seq,
                reason: SkipReason::UnsupportedEvent,
            };
            (update, book_event)
        }
    };

    Ok(AppliedMessage { update, book_event })
}

pub struct ReplayMessage<'a> {
    pub msg: StreamMessageRef<'a>,
    pub update: ReplayUpdate,
    pub book_event: Option<BookEvent<'a>>,
    pub book: &'a L2Book,
}

#[derive(Debug, Clone, Copy)]
pub struct BookEvent<'a> {
    pub header: BookEventHeader,
    pub payload: BookEventPayload<'a>,
}

#[derive(Debug, Clone, Copy)]
pub enum BookEventPayload<'a> {
    Snapshot {
        snapshot: L2Snapshot,
        levels: LevelsView<'a>,
    },
    Diff {
        diff: L2Diff,
        levels: LevelsView<'a>,
    },
    Reset,
    Heartbeat,
    Unsupported,
}

#[derive(Debug, Clone, Copy)]
pub struct LevelsView<'a> {
    buf: &'a [u8],
    offset: usize,
    bid_count: usize,
    ask_count: usize,
}

impl<'a> LevelsView<'a> {
    pub fn new(buf: &'a [u8], offset: usize, bid_count: usize, ask_count: usize) -> Option<Self> {
        let size = std::mem::size_of::<PriceLevelUpdate>();
        let total = bid_count.checked_add(ask_count)?;
        let total_bytes = total.checked_mul(size)?;
        if buf.len() < offset + total_bytes {
            return None;
        }
        Some(Self {
            buf,
            offset,
            bid_count,
            ask_count,
        })
    }

    pub fn bids_iter(&self) -> PriceLevelIter<'a> {
        PriceLevelIter::new(self.buf, self.offset, self.bid_count)
    }

    pub fn asks_iter(&self) -> PriceLevelIter<'a> {
        let size = std::mem::size_of::<PriceLevelUpdate>();
        let ask_offset = self.offset + self.bid_count * size;
        PriceLevelIter::new(self.buf, ask_offset, self.ask_count)
    }
}

pub struct PriceLevelIter<'a> {
    buf: &'a [u8],
    offset: usize,
    remaining: usize,
}

impl<'a> PriceLevelIter<'a> {
    fn new(buf: &'a [u8], offset: usize, count: usize) -> Self {
        Self {
            buf,
            offset,
            remaining: count,
        }
    }
}

impl<'a> Iterator for PriceLevelIter<'a> {
    type Item = PriceLevelUpdate;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let item = read_copy::<PriceLevelUpdate>(self.buf, self.offset)?;
        self.offset += std::mem::size_of::<PriceLevelUpdate>();
        self.remaining -= 1;
        Some(item)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ReplayPolicy {
    pub gap: GapPolicy,
}

impl Default for ReplayPolicy {
    fn default() -> Self {
        Self {
            gap: GapPolicy::Panic,
        }
    }
}

#[derive(Debug)]
pub enum ReplayUpdate {
    Applied { seq: u64, event_type: BookEventType },
    Skipped { seq: u64, reason: SkipReason },
}

#[derive(Debug)]
pub enum SkipReason {
    NonBookEvent,
    UnsupportedMode,
    UnsupportedEvent,
    Corrupt,
    Gap,
}

#[derive(Debug, Default)]
pub struct L2Book {
    bids: BTreeMap<u64, u64>,
    asks: BTreeMap<u64, u64>,
    price_scale: u8,
    size_scale: u8,
}

impl L2Book {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bids(&self) -> &BTreeMap<u64, u64> {
        &self.bids
    }

    pub fn asks(&self) -> &BTreeMap<u64, u64> {
        &self.asks
    }

    pub fn scales(&self) -> (u8, u8) {
        (self.price_scale, self.size_scale)
    }

    pub fn apply_snapshot<I, J>(&mut self, snapshot: &L2Snapshot, bids: I, asks: J)
    where
        I: IntoIterator<Item = PriceLevelUpdate>,
        J: IntoIterator<Item = PriceLevelUpdate>,
    {
        self.price_scale = snapshot.price_scale;
        self.size_scale = snapshot.size_scale;
        self.bids.clear();
        self.asks.clear();
        for level in bids {
            if level.size != 0 {
                self.bids.insert(level.price, level.size);
            }
        }
        for level in asks {
            if level.size != 0 {
                self.asks.insert(level.price, level.size);
            }
        }
    }

    pub fn apply_diff<I, J>(&mut self, diff: &L2Diff, bids: I, asks: J)
    where
        I: IntoIterator<Item = PriceLevelUpdate>,
        J: IntoIterator<Item = PriceLevelUpdate>,
    {
        self.price_scale = diff.price_scale;
        self.size_scale = diff.size_scale;
        for level in bids {
            if level.size == 0 {
                self.bids.remove(&level.price);
            } else {
                self.bids.insert(level.price, level.size);
            }
        }
        for level in asks {
            if level.size == 0 {
                self.asks.remove(&level.price);
            } else {
                self.asks.insert(level.price, level.size);
            }
        }
    }
}

fn read_copy<T: Copy>(buf: &[u8], offset: usize) -> Option<T> {
    let size = std::mem::size_of::<T>();
    if buf.len() < offset + size {
        return None;
    }
    let ptr = unsafe { buf.as_ptr().add(offset) as *const T };
    Some(unsafe { std::ptr::read_unaligned(ptr) })
}
