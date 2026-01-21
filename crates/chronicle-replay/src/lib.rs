use anyhow::{anyhow, bail, Result};
use chronicle_core::Queue;
use chronicle_core::{MessageView, QueueReader, ReaderConfig, WaitStrategy};
use chronicle_protocol::{
    BookEventHeader, BookEventType, BookMode, L2Diff, L2Snapshot, PriceLevelUpdate, TypeId,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub mod snapshot;

pub struct ReplayEngine {
    root: PathBuf,
    reader: QueueReader,
    policy: ReplayPolicy,
    l2_book: L2Book,
    last_seq: Option<u64>,
    last_gap: Option<(u64, u64)>,
}

impl ReplayEngine {
    pub fn open(path: impl AsRef<Path>, reader: &str) -> Result<Self> {
        let root = path.as_ref().to_path_buf();
        let reader = Queue::open_subscriber(&root, reader)?;
        Ok(Self {
            root,
            reader,
            policy: ReplayPolicy::default(),
            l2_book: L2Book::new(),
            last_seq: None,
            last_gap: None,
        })
    }

    pub fn open_with_config(path: impl AsRef<Path>, reader: &str, config: ReaderConfig) -> Result<Self> {
        let root = path.as_ref().to_path_buf();
        let reader = Queue::open_subscriber_with_config(&root, reader, config)?;
        Ok(Self {
            root,
            reader,
            policy: ReplayPolicy::default(),
            l2_book: L2Book::new(),
            last_seq: None,
            last_gap: None,
        })
    }

    pub fn set_wait_strategy(&mut self, strategy: WaitStrategy) {
        self.reader.set_wait_strategy(strategy);
    }

    pub fn policy_mut(&mut self) -> &mut ReplayPolicy {
        &mut self.policy
    }

    pub fn l2_book(&self) -> &L2Book {
        &self.l2_book
    }

    pub fn last_gap(&self) -> Option<(u64, u64)> {
        self.last_gap
    }

    pub fn next(&mut self) -> Result<Option<ReplayUpdate>> {
        let Some(msg) = self.reader.next()? else {
            return Ok(None);
        };

        let applied = apply_message(
            &self.policy,
            &mut self.last_seq,
            &mut self.last_gap,
            &mut self.l2_book,
            &msg,
        )?;
        Ok(Some(applied.update))
    }

    pub fn wait(&mut self, timeout: Option<std::time::Duration>) -> Result<()> {
        self.reader.wait(timeout).map_err(|e| e.into())
    }

    pub fn commit(&mut self) -> Result<()> {
        self.reader.commit().map_err(|e| e.into())
    }

    pub fn warm_start_from_snapshot_path(&mut self, path: impl AsRef<Path>) -> Result<snapshot::SnapshotHeader> {
        let (header, payload) = snapshot::load_snapshot(path.as_ref())?;
        self.apply_snapshot_payload(&header, &payload)?;
        self.last_seq = Some(header.seq_num);
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

struct AppliedMessage {
    update: ReplayUpdate,
    book_header: Option<BookEventHeader>,
}

impl ReplayEngine {
    fn apply_snapshot_payload(
        &mut self,
        header: &snapshot::SnapshotHeader,
        payload: &[u8],
    ) -> Result<()> {
        if header.schema_version != snapshot::SNAPSHOT_VERSION {
            bail!("unsupported snapshot schema version {}", header.schema_version);
        }
        if header.book_mode != BookMode::L2 as u16 {
            bail!("unsupported snapshot book mode {}", header.book_mode);
        }
        let Some(snapshot) = read_copy::<L2Snapshot>(payload, 0) else {
            bail!("snapshot payload too small for L2Snapshot");
        };
        let levels_offset = std::mem::size_of::<L2Snapshot>();
        let total = snapshot.bid_count as usize + snapshot.ask_count as usize;
        let Some(levels) = read_levels(payload, levels_offset, total) else {
            bail!("snapshot payload missing price levels");
        };
        let (bids, asks) = levels.split_at(snapshot.bid_count as usize);
        self.l2_book.apply_snapshot(&snapshot, bids, asks);
        Ok(())
    }

    fn replay_until_exchange_ts(&mut self, target_ts_ns: u64) -> Result<bool> {
        loop {
            let Some(msg) = self.reader.next()? else {
                return Ok(false);
            };
            let applied = apply_message(
                &self.policy,
                &mut self.last_seq,
                &mut self.last_gap,
                &mut self.l2_book,
                &msg,
            )?;
            if let Some(header) = applied.book_header {
                if header.exchange_ts_ns >= target_ts_ns {
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
                &self.policy,
                &mut self.last_seq,
                &mut self.last_gap,
                &mut self.l2_book,
                &msg,
            )?;
            if let Some(header) = applied.book_header {
                if header.ingest_ts_ns >= target_ts_ns {
                    return Ok(true);
                }
            }
        }
    }
}

fn apply_message(
    policy: &ReplayPolicy,
    last_seq: &mut Option<u64>,
    last_gap: &mut Option<(u64, u64)>,
    l2_book: &mut L2Book,
    msg: &MessageView<'_>,
) -> Result<AppliedMessage> {
    if let Some(prev) = *last_seq {
        if msg.seq != prev.wrapping_add(1) {
            *last_gap = Some((prev, msg.seq));
            match policy.gap {
                GapPolicy::Panic => {
                    return Err(anyhow!("sequence gap: {} -> {}", prev, msg.seq));
                }
                GapPolicy::Quarantine => {
                    *last_seq = Some(msg.seq);
                    return Ok(AppliedMessage {
                        update: ReplayUpdate::Skipped {
                            seq: msg.seq,
                            reason: SkipReason::Gap,
                        },
                        book_header: None,
                    });
                }
                GapPolicy::Ignore => {
                    *last_seq = Some(msg.seq);
                }
            }
        }
    }
    *last_seq = Some(msg.seq);

    if msg.type_id != TypeId::BookEvent.as_u16() {
        return Ok(AppliedMessage {
            update: ReplayUpdate::Skipped {
                seq: msg.seq,
                reason: SkipReason::NonBookEvent,
            },
            book_header: None,
        });
    }

    let Some(header) = read_copy::<BookEventHeader>(msg.payload, 0) else {
        return Ok(AppliedMessage {
            update: ReplayUpdate::Skipped {
                seq: msg.seq,
                reason: SkipReason::Corrupt,
            },
            book_header: None,
        });
    };

    if header.book_mode != BookMode::L2 as u8 {
        return Ok(AppliedMessage {
            update: ReplayUpdate::Skipped {
                seq: msg.seq,
                reason: SkipReason::UnsupportedMode,
            },
            book_header: Some(header),
        });
    }

    let update = match header.event_type {
        x if x == BookEventType::Snapshot as u8 => {
            let offset = std::mem::size_of::<BookEventHeader>();
            let Some(snapshot) = read_copy::<L2Snapshot>(msg.payload, offset) else {
                return Ok(AppliedMessage {
                    update: ReplayUpdate::Skipped {
                        seq: msg.seq,
                        reason: SkipReason::Corrupt,
                    },
                    book_header: Some(header),
                });
            };
            let levels_offset = offset + std::mem::size_of::<L2Snapshot>();
            let total = snapshot.bid_count as usize + snapshot.ask_count as usize;
            let Some(levels) = read_levels(msg.payload, levels_offset, total) else {
                return Ok(AppliedMessage {
                    update: ReplayUpdate::Skipped {
                        seq: msg.seq,
                        reason: SkipReason::Corrupt,
                    },
                    book_header: Some(header),
                });
            };
            let (bids, asks) = levels.split_at(snapshot.bid_count as usize);
            l2_book.apply_snapshot(&snapshot, bids, asks);
            ReplayUpdate::Applied {
                seq: msg.seq,
                event_type: BookEventType::Snapshot,
            }
        }
        x if x == BookEventType::Diff as u8 => {
            let offset = std::mem::size_of::<BookEventHeader>();
            let Some(diff) = read_copy::<L2Diff>(msg.payload, offset) else {
                return Ok(AppliedMessage {
                    update: ReplayUpdate::Skipped {
                        seq: msg.seq,
                        reason: SkipReason::Corrupt,
                    },
                    book_header: Some(header),
                });
            };
            let levels_offset = offset + std::mem::size_of::<L2Diff>();
            let total = diff.bid_count as usize + diff.ask_count as usize;
            let Some(levels) = read_levels(msg.payload, levels_offset, total) else {
                return Ok(AppliedMessage {
                    update: ReplayUpdate::Skipped {
                        seq: msg.seq,
                        reason: SkipReason::Corrupt,
                    },
                    book_header: Some(header),
                });
            };
            let (bids, asks) = levels.split_at(diff.bid_count as usize);
            l2_book.apply_diff(&diff, bids, asks);
            ReplayUpdate::Applied {
                seq: msg.seq,
                event_type: BookEventType::Diff,
            }
        }
        _ => ReplayUpdate::Skipped {
            seq: msg.seq,
            reason: SkipReason::UnsupportedEvent,
        },
    };

    Ok(AppliedMessage {
        update,
        book_header: Some(header),
    })
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

#[derive(Clone, Copy, Debug)]
pub enum GapPolicy {
    Panic,
    Quarantine,
    Ignore,
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

    pub fn apply_snapshot(&mut self, snapshot: &L2Snapshot, bids: &[PriceLevelUpdate], asks: &[PriceLevelUpdate]) {
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

    pub fn apply_diff(&mut self, diff: &L2Diff, bids: &[PriceLevelUpdate], asks: &[PriceLevelUpdate]) {
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

fn read_levels(buf: &[u8], offset: usize, count: usize) -> Option<Vec<PriceLevelUpdate>> {
    let size = std::mem::size_of::<PriceLevelUpdate>();
    let total = count.checked_mul(size)?;
    if buf.len() < offset + total {
        return None;
    }
    let mut levels = Vec::with_capacity(count);
    for i in 0..count {
        let item_offset = offset + i * size;
        levels.push(read_copy::<PriceLevelUpdate>(buf, item_offset)?);
    }
    Some(levels)
}
