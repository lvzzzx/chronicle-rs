use anyhow::{anyhow, bail, Result};
use blake3::Hasher;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::hash::{Hash, Hasher as _};
use std::path::Path;

use crate::core::MessageView;
use crate::protocol::{
    l3_flags, BookEventHeader, BookEventType, BookMode, L3Event, L3EventType, L3Side, TypeId,
};

#[derive(Debug, Clone, Copy)]
pub enum GapPolicy {
    Fail,
    Ignore,
}

#[derive(Debug, Clone, Copy)]
pub enum DecodePolicy {
    Fail,
    SkipNonL3,
}

#[derive(Debug, Clone, Copy)]
pub enum UnknownOrderPolicy {
    Fail,
    Skip,
}

#[derive(Debug, Clone, Copy)]
pub struct ReconstructPolicy {
    pub gap: GapPolicy,
    pub decode: DecodePolicy,
    pub unknown_order: UnknownOrderPolicy,
}

impl Default for ReconstructPolicy {
    fn default() -> Self {
        Self {
            gap: GapPolicy::Fail,
            decode: DecodePolicy::Fail,
            unknown_order: UnknownOrderPolicy::Fail,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ApplyStatus {
    Applied,
    Skipped,
}

#[derive(Debug, Clone, Copy)]
pub struct L3Message {
    pub header: BookEventHeader,
    pub event: L3Event,
}

#[derive(Debug)]
pub struct ChannelSequencer {
    last_seq: Option<u64>,
    policy: GapPolicy,
}

impl ChannelSequencer {
    pub fn new(policy: GapPolicy) -> Self {
        Self {
            last_seq: None,
            policy,
        }
    }

    pub fn last_seq(&self) -> Option<u64> {
        self.last_seq
    }

    pub fn set_policy(&mut self, policy: GapPolicy) {
        self.policy = policy;
    }

    pub fn check(&mut self, seq: u64) -> Result<()> {
        if let Some(prev) = self.last_seq {
            if seq != prev.wrapping_add(1) {
                match self.policy {
                    GapPolicy::Fail => {
                        bail!("sequence gap: {} -> {}", prev, seq);
                    }
                    GapPolicy::Ignore => {}
                }
            }
        }
        self.last_seq = Some(seq);
        Ok(())
    }
}

#[derive(Debug)]
pub struct SzseL3Engine {
    policy: ReconstructPolicy,
    sequencer: ChannelSequencer,
    dispatcher: SzseL3Dispatcher,
    channel_id: Option<u32>,
}

impl SzseL3Engine {
    pub fn new(worker_count: usize) -> Self {
        let policy = ReconstructPolicy::default();
        let sequencer = ChannelSequencer::new(policy.gap);
        let dispatcher = SzseL3Dispatcher::new(worker_count);
        Self {
            policy,
            sequencer,
            dispatcher,
            channel_id: None,
        }
    }

    pub fn policy_mut(&mut self) -> &mut ReconstructPolicy {
        &mut self.policy
    }

    pub fn set_channel_id(&mut self, channel_id: u32) {
        self.channel_id = Some(channel_id);
    }

    pub fn dispatcher(&self) -> &SzseL3Dispatcher {
        &self.dispatcher
    }

    pub fn dispatcher_mut(&mut self) -> &mut SzseL3Dispatcher {
        &mut self.dispatcher
    }

    pub fn apply_message(&mut self, msg: &MessageView<'_>) -> Result<ApplyStatus> {
        let Some(l3) = decode_l3_message(msg, self.policy.decode)? else {
            return Ok(ApplyStatus::Skipped);
        };

        if let Some(expected) = self.channel_id {
            if l3.header.stream_id != expected {
                bail!(
                    "unexpected channel {}, expected {}",
                    l3.header.stream_id,
                    expected
                );
            }
        } else {
            self.channel_id = Some(l3.header.stream_id);
        }

        self.sequencer.set_policy(self.policy.gap);
        self.sequencer.check(l3.header.seq)?;
        self.dispatcher.route(&l3, self.policy.unknown_order)?;
        Ok(ApplyStatus::Applied)
    }

    pub fn checkpoint(&self) -> Option<ChannelCheckpoint> {
        let channel = self.channel_id?;
        let last_seq = self.sequencer.last_seq?;
        let mut symbols = Vec::new();
        for worker in &self.dispatcher.workers {
            for (symbol, book) in &worker.books {
                symbols.push(SymbolCheckpoint {
                    symbol: *symbol,
                    book_hash: book.hash(),
                });
            }
        }
        symbols.sort_by_key(|entry| entry.symbol);
        Some(ChannelCheckpoint {
            channel,
            last_seq,
            symbols,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct SymbolCheckpoint {
    pub symbol: u32,
    pub book_hash: [u8; 16],
}

#[derive(Debug, Serialize)]
pub struct ChannelCheckpoint {
    pub channel: u32,
    pub last_seq: u64,
    pub symbols: Vec<SymbolCheckpoint>,
}

pub fn write_checkpoint_json(path: impl AsRef<Path>, checkpoint: &ChannelCheckpoint) -> Result<()> {
    let file = std::fs::File::create(path.as_ref())?;
    serde_json::to_writer_pretty(file, checkpoint)?;
    Ok(())
}

pub fn decode_l3_message(msg: &MessageView<'_>, policy: DecodePolicy) -> Result<Option<L3Message>> {
    if msg.type_id != TypeId::BookEvent.as_u16() {
        return match policy {
            DecodePolicy::SkipNonL3 => Ok(None),
            DecodePolicy::Fail => Err(anyhow!("non-book event type {}", msg.type_id)),
        };
    }

    let header = read_copy::<BookEventHeader>(msg.payload, 0)
        .ok_or_else(|| anyhow!("message too small for BookEventHeader"))?;

    if header.book_mode != BookMode::L3 as u8 {
        return match policy {
            DecodePolicy::SkipNonL3 => Ok(None),
            DecodePolicy::Fail => Err(anyhow!("unsupported book mode {}", header.book_mode)),
        };
    }

    if header.event_type != BookEventType::Diff as u8 {
        return match policy {
            DecodePolicy::SkipNonL3 => Ok(None),
            DecodePolicy::Fail => Err(anyhow!("unsupported event type {}", header.event_type)),
        };
    }

    let expected_len =
        std::mem::size_of::<BookEventHeader>() + std::mem::size_of::<L3Event>();
    if header.record_len as usize != expected_len {
        return Err(anyhow!(
            "record_len mismatch: header={} expected={}",
            header.record_len,
            expected_len
        ));
    }
    if msg.payload.len() < expected_len {
        return Err(anyhow!(
            "payload too short: {} < {}",
            msg.payload.len(),
            expected_len
        ));
    }

    let payload_offset = std::mem::size_of::<BookEventHeader>();
    let event = read_copy::<L3Event>(msg.payload, payload_offset)
        .ok_or_else(|| anyhow!("message too small for L3Event"))?;
    Ok(Some(L3Message { header, event }))
}

#[derive(Debug, Default)]
pub struct SzseL3Dispatcher {
    workers: Vec<SzseL3Worker>,
}

impl SzseL3Dispatcher {
    pub fn new(worker_count: usize) -> Self {
        let count = if worker_count == 0 { 1 } else { worker_count };
        let workers = (0..count).map(|_| SzseL3Worker::default()).collect();
        Self { workers }
    }

    pub fn workers(&self) -> &[SzseL3Worker] {
        &self.workers
    }

    pub fn workers_mut(&mut self) -> &mut [SzseL3Worker] {
        &mut self.workers
    }

    pub fn route(&mut self, msg: &L3Message, policy: UnknownOrderPolicy) -> Result<()> {
        let symbol = msg.header.market_id;
        let idx = worker_index(symbol, self.workers.len());
        self.workers[idx].apply_message(&msg.header, &msg.event, policy)
    }
}

#[derive(Debug, Default)]
pub struct SzseL3Worker {
    books: HashMap<u32, L3Book>,
}

impl SzseL3Worker {
    pub fn book(&self, symbol: u32) -> Option<&L3Book> {
        self.books.get(&symbol)
    }

    pub fn book_mut(&mut self, symbol: u32) -> &mut L3Book {
        self.books.entry(symbol).or_insert_with(L3Book::new)
    }

    pub fn apply_message(
        &mut self,
        header: &BookEventHeader,
        event: &L3Event,
        policy: UnknownOrderPolicy,
    ) -> Result<()> {
        let book = self.book_mut(header.market_id);
        book.apply_event(header, event, policy)
    }

    pub fn checkpoints(&self) -> Vec<SymbolCheckpoint> {
        let mut out: Vec<SymbolCheckpoint> = self
            .books
            .iter()
            .map(|(symbol, book)| SymbolCheckpoint {
                symbol: *symbol,
                book_hash: book.hash(),
            })
            .collect();
        out.sort_by_key(|entry| entry.symbol);
        out
    }
}

#[derive(Debug, Clone)]
pub struct L3Order {
    side: L3Side,
    price: u64,
    qty: u64,
    flags: u32,
    ord_type: u8,
    last_exchange_ts_ns: u64,
}

impl L3Order {
    pub fn side(&self) -> L3Side {
        self.side
    }

    pub fn price(&self) -> u64 {
        self.price
    }

    pub fn qty(&self) -> u64 {
        self.qty
    }

    pub fn flags(&self) -> u32 {
        self.flags
    }

    pub fn ord_type(&self) -> u8 {
        self.ord_type
    }

    pub fn last_exchange_ts_ns(&self) -> u64 {
        self.last_exchange_ts_ns
    }
}

#[derive(Debug, Default, Clone)]
pub struct PriceLevel {
    total_qty: u64,
    queue: VecDeque<u64>,
}

impl PriceLevel {
    pub fn total_qty(&self) -> u64 {
        self.total_qty
    }

    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    fn add(&mut self, order_id: u64, qty: u64) {
        self.queue.push_back(order_id);
        self.total_qty = self.total_qty.saturating_add(qty);
    }

    fn remove_qty(&mut self, order_id: u64, qty: u64) {
        self.total_qty = self.total_qty.saturating_sub(qty);
        if let Some(pos) = self.queue.iter().position(|id| *id == order_id) {
            self.queue.remove(pos);
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct L3Book {
    orders: HashMap<u64, L3Order>,
    bids: BTreeMap<u64, PriceLevel>,
    asks: BTreeMap<u64, PriceLevel>,
    price_scale: u8,
    size_scale: u8,
}

impl L3Book {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bids(&self) -> &BTreeMap<u64, PriceLevel> {
        &self.bids
    }

    pub fn asks(&self) -> &BTreeMap<u64, PriceLevel> {
        &self.asks
    }

    pub fn orders(&self) -> &HashMap<u64, L3Order> {
        &self.orders
    }

    pub fn scales(&self) -> (u8, u8) {
        (self.price_scale, self.size_scale)
    }

    pub fn hash(&self) -> [u8; 16] {
        let mut hasher = Hasher::new();
        hasher.update(&[self.price_scale, self.size_scale]);
        let mut order_ids: Vec<u64> = self.orders.keys().copied().collect();
        order_ids.sort_unstable();
        for order_id in order_ids {
            if let Some(order) = self.orders.get(&order_id) {
                hasher.update(&order_id.to_le_bytes());
                hasher.update(&[order.side as u8, order.ord_type]);
                hasher.update(&order.price.to_le_bytes());
                hasher.update(&order.qty.to_le_bytes());
                hasher.update(&order.flags.to_le_bytes());
                hasher.update(&order.last_exchange_ts_ns.to_le_bytes());
            }
        }
        let digest = hasher.finalize();
        let mut out = [0u8; 16];
        out.copy_from_slice(&digest.as_bytes()[..16]);
        out
    }

    fn apply_event(
        &mut self,
        header: &BookEventHeader,
        event: &L3Event,
        policy: UnknownOrderPolicy,
    ) -> Result<()> {
        self.price_scale = event.price_scale;
        self.size_scale = event.size_scale;
        let event_type = match event.event_type {
            x if x == L3EventType::OrderAdd as u8 => L3EventType::OrderAdd,
            x if x == L3EventType::OrderCancel as u8 => L3EventType::OrderCancel,
            x if x == L3EventType::OrderModify as u8 => L3EventType::OrderModify,
            x if x == L3EventType::Trade as u8 => L3EventType::Trade,
            x if x == L3EventType::Reset as u8 => L3EventType::Reset,
            x if x == L3EventType::Heartbeat as u8 => L3EventType::Heartbeat,
            _ => {
                return Err(anyhow!("unknown L3 event type {}", event.event_type));
            }
        };

        match event_type {
            L3EventType::OrderAdd => self.on_order_add(header, event),
            L3EventType::OrderCancel => self.on_order_cancel(header, event, policy),
            L3EventType::OrderModify => self.on_order_modify(header, event, policy),
            L3EventType::Trade => self.on_trade(header, event, policy),
            L3EventType::Reset => {
                self.orders.clear();
                self.bids.clear();
                self.asks.clear();
                Ok(())
            }
            L3EventType::Heartbeat => Ok(()),
        }
    }

    fn on_order_add(&mut self, header: &BookEventHeader, event: &L3Event) -> Result<()> {
        let order_id = event.order_id;
        if order_id == 0 {
            bail!("order add missing order_id");
        }
        let side = match event.side {
            x if x == L3Side::Buy as u8 => L3Side::Buy,
            x if x == L3Side::Sell as u8 => L3Side::Sell,
            _ => L3Side::Unknown,
        };
        let order = L3Order {
            side,
            price: event.price,
            qty: event.qty,
            flags: event.flags,
            ord_type: event.ord_type,
            last_exchange_ts_ns: header.exchange_ts_ns,
        };
        self.orders.insert(order_id, order);

        if event.flags & l3_flags::PRICE_IS_MARKET != 0 || event.price == 0 {
            return Ok(());
        }

        let level = match side {
            L3Side::Buy => self.bids.entry(event.price).or_default(),
            L3Side::Sell => self.asks.entry(event.price).or_default(),
            L3Side::Unknown => return Ok(()),
        };
        level.add(order_id, event.qty);
        Ok(())
    }

    fn on_order_cancel(
        &mut self,
        header: &BookEventHeader,
        event: &L3Event,
        policy: UnknownOrderPolicy,
    ) -> Result<()> {
        let order_id = event.order_id;
        if order_id == 0 {
            return Ok(());
        }
        let Some(mut order) = self.orders.remove(&order_id) else {
            return match policy {
                UnknownOrderPolicy::Fail => Err(anyhow!("cancel unknown order {}", order_id)),
                UnknownOrderPolicy::Skip => Ok(()),
            };
        };
        order.last_exchange_ts_ns = header.exchange_ts_ns;
        let cancel_qty = if event.qty == 0 {
            order.qty
        } else {
            event.qty.min(order.qty)
        };
        order.qty = order.qty.saturating_sub(cancel_qty);
        self.remove_from_level(order.side, order.price, order_id, cancel_qty);
        if order.qty != 0 {
            self.orders.insert(order_id, order);
        }
        Ok(())
    }

    fn on_order_modify(
        &mut self,
        header: &BookEventHeader,
        event: &L3Event,
        policy: UnknownOrderPolicy,
    ) -> Result<()> {
        let order_id = event.order_id;
        if order_id == 0 {
            return Ok(());
        }
        let Some(mut order) = self.orders.remove(&order_id) else {
            return match policy {
                UnknownOrderPolicy::Fail => Err(anyhow!("modify unknown order {}", order_id)),
                UnknownOrderPolicy::Skip => Ok(()),
            };
        };
        let old_price = order.price;
        let old_side = order.side;
        let old_qty = order.qty;
        order.last_exchange_ts_ns = header.exchange_ts_ns;

        if event.qty != 0 {
            order.qty = event.qty;
        }
        if event.price != 0 {
            order.price = event.price;
        }

        if event.flags & l3_flags::PRICE_IS_MARKET != 0 || order.price == 0 {
            if old_qty != 0 {
                self.remove_from_level(old_side, old_price, order_id, old_qty);
            }
            self.orders.insert(order_id, order);
            return Ok(());
        }

        if old_price != order.price || old_side != order.side {
            self.remove_from_level(old_side, old_price, order_id, old_qty);
            self.add_to_level(order.side, order.price, order_id, order.qty);
        } else if old_qty != order.qty {
            let level = match order.side {
                L3Side::Buy => self.bids.entry(order.price).or_default(),
                L3Side::Sell => self.asks.entry(order.price).or_default(),
                L3Side::Unknown => {
                    self.orders.insert(order_id, order);
                    return Ok(());
                }
            };
            if order.qty > old_qty {
                level.total_qty = level.total_qty.saturating_add(order.qty - old_qty);
            } else {
                level.total_qty = level.total_qty.saturating_sub(old_qty - order.qty);
            }
        }
        self.orders.insert(order_id, order);
        Ok(())
    }

    fn on_trade(
        &mut self,
        header: &BookEventHeader,
        event: &L3Event,
        policy: UnknownOrderPolicy,
    ) -> Result<()> {
        let qty = event.qty;
        let mut applied = false;
        if event.bid_order_id != 0 {
            self.decrement_order(header, event.bid_order_id, qty, policy)?;
            applied = true;
        }
        if event.ask_order_id != 0 {
            self.decrement_order(header, event.ask_order_id, qty, policy)?;
            applied = true;
        }
        if !applied && event.order_id != 0 {
            self.decrement_order(header, event.order_id, qty, policy)?;
        }
        Ok(())
    }

    fn decrement_order(
        &mut self,
        header: &BookEventHeader,
        order_id: u64,
        qty: u64,
        policy: UnknownOrderPolicy,
    ) -> Result<()> {
        let Some(mut order) = self.orders.remove(&order_id) else {
            return match policy {
                UnknownOrderPolicy::Fail => Err(anyhow!("trade unknown order {}", order_id)),
                UnknownOrderPolicy::Skip => Ok(()),
            };
        };
        order.last_exchange_ts_ns = header.exchange_ts_ns;
        let trade_qty = if qty == 0 { order.qty } else { qty.min(order.qty) };
        order.qty = order.qty.saturating_sub(trade_qty);
        self.remove_from_level(order.side, order.price, order_id, trade_qty);
        if order.qty != 0 {
            self.orders.insert(order_id, order);
        }
        Ok(())
    }

    fn add_to_level(&mut self, side: L3Side, price: u64, order_id: u64, qty: u64) {
        let level = match side {
            L3Side::Buy => self.bids.entry(price).or_default(),
            L3Side::Sell => self.asks.entry(price).or_default(),
            L3Side::Unknown => return,
        };
        level.add(order_id, qty);
    }

    fn remove_from_level(&mut self, side: L3Side, price: u64, order_id: u64, qty: u64) {
        let level = match side {
            L3Side::Buy => self.bids.get_mut(&price),
            L3Side::Sell => self.asks.get_mut(&price),
            L3Side::Unknown => None,
        };
        if let Some(level) = level {
            level.remove_qty(order_id, qty);
            if level.total_qty == 0 {
                match side {
                    L3Side::Buy => {
                        self.bids.remove(&price);
                    }
                    L3Side::Sell => {
                        self.asks.remove(&price);
                    }
                    L3Side::Unknown => {}
                }
            }
        }
    }
}

fn worker_index(symbol: u32, worker_count: usize) -> usize {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    symbol.hash(&mut hasher);
    (hasher.finish() as usize) % worker_count
}

fn read_copy<T: Copy>(buf: &[u8], offset: usize) -> Option<T> {
    let size = std::mem::size_of::<T>();
    if buf.len() < offset + size {
        return None;
    }
    let ptr = unsafe { buf.as_ptr().add(offset) as *const T };
    Some(unsafe { std::ptr::read_unaligned(ptr) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(seq: u64) -> BookEventHeader {
        BookEventHeader {
            schema_version: 3,
            record_len: 0,
            endianness: 0,
            _pad0: 0,
            venue_id: 0,
            market_id: 1,
            stream_id: 2011,
            ingest_ts_ns: 0,
            exchange_ts_ns: 1,
            seq,
            native_seq: seq,
            event_type: BookEventType::Diff as u8,
            book_mode: BookMode::L3 as u8,
            flags: 0,
            _pad1: 0,
        }
    }

    fn add_event(order_id: u64, side: L3Side, price: u64, qty: u64) -> L3Event {
        L3Event {
            event_type: L3EventType::OrderAdd as u8,
            side: side as u8,
            ord_type: 0,
            exec_type: 0,
            price_scale: 3,
            size_scale: 0,
            _pad0: 0,
            flags: 0,
            _pad1: 0,
            order_id,
            bid_order_id: 0,
            ask_order_id: 0,
            price,
            qty,
            amt: 0,
        }
    }

    #[test]
    fn l3_book_add_cancel() {
        let mut book = L3Book::new();
        let header = header(1);
        let add = add_event(10, L3Side::Buy, 100, 5);
        book.apply_event(&header, &add, UnknownOrderPolicy::Fail)
            .unwrap();
        assert_eq!(book.orders.len(), 1);
        assert_eq!(book.bids().get(&100).unwrap().total_qty(), 5);

        let cancel = L3Event {
            event_type: L3EventType::OrderCancel as u8,
            side: L3Side::Buy as u8,
            ord_type: 0,
            exec_type: 0,
            price_scale: 3,
            size_scale: 0,
            _pad0: 0,
            flags: 0,
            _pad1: 0,
            order_id: 10,
            bid_order_id: 0,
            ask_order_id: 0,
            price: 100,
            qty: 5,
            amt: 0,
        };
        book.apply_event(&header, &cancel, UnknownOrderPolicy::Fail)
            .unwrap();
        assert!(book.orders.is_empty());
        assert!(book.bids().is_empty());
    }

    #[test]
    fn l3_book_trade_both_sides() {
        let mut book = L3Book::new();
        let header = header(1);
        let bid = add_event(1, L3Side::Buy, 100, 10);
        let ask = add_event(2, L3Side::Sell, 101, 10);
        book.apply_event(&header, &bid, UnknownOrderPolicy::Fail)
            .unwrap();
        book.apply_event(&header, &ask, UnknownOrderPolicy::Fail)
            .unwrap();

        let trade = L3Event {
            event_type: L3EventType::Trade as u8,
            side: L3Side::Unknown as u8,
            ord_type: 0,
            exec_type: 0,
            price_scale: 3,
            size_scale: 0,
            _pad0: 0,
            flags: 0,
            _pad1: 0,
            order_id: 0,
            bid_order_id: 1,
            ask_order_id: 2,
            price: 100,
            qty: 5,
            amt: 0,
        };
        book.apply_event(&header, &trade, UnknownOrderPolicy::Fail)
            .unwrap();
        assert_eq!(book.orders.get(&1).unwrap().qty(), 5);
        assert_eq!(book.orders.get(&2).unwrap().qty(), 5);
    }
}
