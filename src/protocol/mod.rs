#![allow(clippy::upper_case_acronyms)]

pub const PROTOCOL_VERSION: u16 = 2;

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeId {
    /// Canonical order book replay events (BookEventHeader + payload).
    BookEvent = 0x1000,
    /// Book ticker updates (best bid/ask).
    BookTicker = 0x1001,
    /// Trade events.
    Trade = 0x1002,
}

impl TypeId {
    #[inline]
    pub const fn as_u16(self) -> u16 {
        self as u16
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BookEventType {
    Snapshot = 1,
    Diff = 2,
    Reset = 3,
    Heartbeat = 4,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BookMode {
    L2 = 0,
    L3 = 1,
}

pub mod book_flags {
    pub const ABSOLUTE: u16 = 1 << 0;
    pub const DELTA: u16 = 1 << 1;
    pub const HAS_SCALE: u16 = 1 << 2;
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookEventHeader {
    pub schema_version: u16,
    pub record_len: u32,
    pub endianness: u8,
    pub _pad0: u8,
    pub venue_id: u16,
    pub market_id: u32,
    pub stream_id: u32,
    pub ingest_ts_ns: u64,
    pub exchange_ts_ns: u64,
    pub seq: u64,
    pub native_seq: u64,
    pub event_type: u8,
    pub book_mode: u8,
    pub flags: u16,
    pub _pad1: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BookTicker {
    pub timestamp_ns: u64,
    pub bid_price: f64,
    pub bid_qty: f64,
    pub ask_price: f64,
    pub ask_qty: f64,
    pub symbol_hash: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trade {
    pub timestamp_ns: u64,
    pub price: f64,
    pub qty: f64,
    pub trade_id: u64,
    pub buyer_order_id: u64,
    pub seller_order_id: u64,
    pub is_buyer_maker: bool,
    pub _pad0: [u8; 7],
    pub symbol_hash: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct L2Diff {
    pub update_id_first: u64,
    pub update_id_last: u64,
    pub update_id_prev: u64,
    pub price_scale: u8,
    pub size_scale: u8,
    pub flags: u16,
    pub bid_count: u16,
    pub ask_count: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct L2Snapshot {
    pub price_scale: u8,
    pub size_scale: u8,
    pub _pad0: u16,
    pub bid_count: u32,
    pub ask_count: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceLevelUpdate {
    pub price: u64,
    pub size: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct L3Diff {
    pub price_scale: u8,
    pub size_scale: u8,
    pub action: u8,
    pub side: u8,
    pub _pad0: u8,
    pub _pad1: u8,
    pub order_id: u128,
    pub price: u64,
    pub size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, size_of};

    #[test]
    fn book_event_header_size() {
        assert_eq!(size_of::<BookEventHeader>(), 64);
        assert_eq!(align_of::<BookEventHeader>(), 8);
    }

    #[test]
    fn l2_diff_size() {
        assert_eq!(size_of::<L2Diff>(), 32);
    }

    #[test]
    fn l2_snapshot_size() {
        assert_eq!(size_of::<L2Snapshot>(), 12);
    }

    #[test]
    fn price_level_update_size() {
        assert_eq!(size_of::<PriceLevelUpdate>(), 16);
    }

    #[test]
    fn book_ticker_size() {
        assert_eq!(size_of::<BookTicker>(), 48);
    }

    #[test]
    fn trade_size() {
        assert_eq!(size_of::<Trade>(), 64);
    }
}
