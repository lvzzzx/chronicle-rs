use anyhow::{anyhow, Result};
use chronicle_core::Queue;
use chronicle_core::{QueueReader, ReaderConfig, WaitStrategy};
use chronicle_protocol::{
    BookEventHeader, BookEventType, BookMode, L2Diff, L2Snapshot, PriceLevelUpdate, TypeId,
};
use std::collections::BTreeMap;
use std::path::Path;

pub struct ReplayEngine {
    reader: QueueReader,
    policy: ReplayPolicy,
    l2_book: L2Book,
    last_seq: Option<u64>,
    last_gap: Option<(u64, u64)>,
}

impl ReplayEngine {
    pub fn open(path: impl AsRef<Path>, reader: &str) -> Result<Self> {
        let reader = Queue::open_subscriber(path, reader)?;
        Ok(Self {
            reader,
            policy: ReplayPolicy::default(),
            l2_book: L2Book::new(),
            last_seq: None,
            last_gap: None,
        })
    }

    pub fn open_with_config(path: impl AsRef<Path>, reader: &str, config: ReaderConfig) -> Result<Self> {
        let reader = Queue::open_subscriber_with_config(path, reader, config)?;
        Ok(Self {
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

        if let Some(prev) = self.last_seq {
            if msg.seq != prev.wrapping_add(1) {
                self.last_gap = Some((prev, msg.seq));
                match self.policy.gap {
                    GapPolicy::Panic => {
                        return Err(anyhow!("sequence gap: {} -> {}", prev, msg.seq));
                    }
                    GapPolicy::Quarantine => {
                        self.last_seq = Some(msg.seq);
                        return Ok(Some(ReplayUpdate::Skipped {
                            seq: msg.seq,
                            reason: SkipReason::Gap,
                        }));
                    }
                    GapPolicy::Ignore => {
                        self.last_seq = Some(msg.seq);
                    }
                }
            }
        }
        self.last_seq = Some(msg.seq);

        if msg.type_id != TypeId::BookEvent.as_u16() {
            return Ok(Some(ReplayUpdate::Skipped {
                seq: msg.seq,
                reason: SkipReason::NonBookEvent,
            }));
        }

        let Some(header) = read_copy::<BookEventHeader>(msg.payload, 0) else {
            return Ok(Some(ReplayUpdate::Skipped {
                seq: msg.seq,
                reason: SkipReason::Corrupt,
            }));
        };

        if header.book_mode != BookMode::L2 as u8 {
            return Ok(Some(ReplayUpdate::Skipped {
                seq: msg.seq,
                reason: SkipReason::UnsupportedMode,
            }));
        }

        match header.event_type {
            x if x == BookEventType::Snapshot as u8 => {
                let offset = std::mem::size_of::<BookEventHeader>();
                let Some(snapshot) = read_copy::<L2Snapshot>(msg.payload, offset) else {
                    return Ok(Some(ReplayUpdate::Skipped {
                        seq: msg.seq,
                        reason: SkipReason::Corrupt,
                    }));
                };
                let levels_offset = offset + std::mem::size_of::<L2Snapshot>();
                let total = snapshot.bid_count as usize + snapshot.ask_count as usize;
                let Some(levels) = read_levels(msg.payload, levels_offset, total) else {
                    return Ok(Some(ReplayUpdate::Skipped {
                        seq: msg.seq,
                        reason: SkipReason::Corrupt,
                    }));
                };
                let (bids, asks) = levels.split_at(snapshot.bid_count as usize);
                self.l2_book.apply_snapshot(&snapshot, bids, asks);
                Ok(Some(ReplayUpdate::Applied {
                    seq: msg.seq,
                    event_type: BookEventType::Snapshot,
                }))
            }
            x if x == BookEventType::Diff as u8 => {
                let offset = std::mem::size_of::<BookEventHeader>();
                let Some(diff) = read_copy::<L2Diff>(msg.payload, offset) else {
                    return Ok(Some(ReplayUpdate::Skipped {
                        seq: msg.seq,
                        reason: SkipReason::Corrupt,
                    }));
                };
                let levels_offset = offset + std::mem::size_of::<L2Diff>();
                let total = diff.bid_count as usize + diff.ask_count as usize;
                let Some(levels) = read_levels(msg.payload, levels_offset, total) else {
                    return Ok(Some(ReplayUpdate::Skipped {
                        seq: msg.seq,
                        reason: SkipReason::Corrupt,
                    }));
                };
                let (bids, asks) = levels.split_at(diff.bid_count as usize);
                self.l2_book.apply_diff(&diff, bids, asks);
                Ok(Some(ReplayUpdate::Applied {
                    seq: msg.seq,
                    event_type: BookEventType::Diff,
                }))
            }
            _ => Ok(Some(ReplayUpdate::Skipped {
                seq: msg.seq,
                reason: SkipReason::UnsupportedEvent,
            })),
        }
    }

    pub fn wait(&mut self, timeout: Option<std::time::Duration>) -> Result<()> {
        self.reader.wait(timeout).map_err(|e| e.into())
    }

    pub fn commit(&mut self) -> Result<()> {
        self.reader.commit().map_err(|e| e.into())
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
