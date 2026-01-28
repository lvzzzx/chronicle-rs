#![allow(clippy::upper_case_acronyms)]

pub const PROTOCOL_VERSION: u16 = 3;

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

pub mod l3_flags {
    pub const PRICE_IS_MARKET: u32 = 1 << 0;
    pub const AUCTION_OPEN: u32 = 1 << 1;
    pub const AUCTION_CLOSE: u32 = 1 << 2;
    pub const CANCEL_RESTRICTED_WINDOW: u32 = 1 << 3;
    pub const RAW_HAS_BID_ID: u32 = 1 << 4;
    pub const RAW_HAS_ASK_ID: u32 = 1 << 5;
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum L3EventType {
    OrderAdd = 1,
    OrderCancel = 2,
    OrderModify = 3,
    Trade = 4,
    Reset = 5,
    Heartbeat = 6,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum L3Side {
    Unknown = 0,
    Buy = 1,
    Sell = 2,
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
pub struct L3Event {
    pub event_type: u8,
    pub side: u8,
    pub ord_type: u8,
    pub exec_type: u8,
    pub price_scale: u8,
    pub size_scale: u8,
    pub _pad0: u16,
    pub flags: u32,
    pub _pad1: u32,
    pub order_id: u64,
    pub bid_order_id: u64,
    pub ask_order_id: u64,
    pub price: u64,
    pub qty: u64,
    pub amt: u64,
}

/// Serialization utilities for protocol structures.
///
/// This module provides safe abstractions for serializing protocol structures
/// to byte buffers, encapsulating the unsafe transmute operations required
/// for zero-copy serialization.
pub mod serialization {
    use super::*;

    /// Helper to serialize a POD struct to bytes using transmute.
    ///
    /// # Safety
    ///
    /// This is safe for repr(C) structs with no padding or only explicit padding fields.
    /// The caller must ensure the struct meets these requirements.
    #[inline]
    unsafe fn struct_to_bytes<T>(value: &T) -> &[u8] {
        std::slice::from_raw_parts(value as *const _ as *const u8, std::mem::size_of::<T>())
    }

    /// Writes an L2 snapshot book event to a byte buffer.
    ///
    /// # Arguments
    ///
    /// * `buf` - The destination buffer (must be large enough)
    /// * `venue_id` - Venue/exchange identifier
    /// * `market_id` - Market/symbol identifier
    /// * `timestamp_ns` - Timestamp for the snapshot
    /// * `price_scale` - Price decimal scale
    /// * `size_scale` - Size decimal scale
    /// * `bids` - Iterator of bid levels (price, size) in descending order
    /// * `asks` - Iterator of ask levels (price, size) in ascending order
    ///
    /// # Returns
    ///
    /// The number of bytes written to the buffer.
    ///
    /// # Panics
    ///
    /// Panics if the buffer is too small to hold the complete snapshot.
    pub fn write_l2_snapshot<'a, I, J>(
        buf: &mut [u8],
        venue_id: u16,
        market_id: u32,
        timestamp_ns: u64,
        price_scale: u8,
        size_scale: u8,
        bids: I,
        asks: J,
    ) -> usize
    where
        I: Iterator<Item = (u64, u64)>,
        J: Iterator<Item = (u64, u64)>,
    {
        // Collect bids and asks to calculate counts
        let bids: Vec<_> = bids.collect();
        let asks: Vec<_> = asks.collect();

        let bid_count = bids.len();
        let ask_count = asks.len();

        // Calculate sizes
        let header_size = std::mem::size_of::<BookEventHeader>();
        let snapshot_size = std::mem::size_of::<L2Snapshot>();
        let level_size = std::mem::size_of::<PriceLevelUpdate>();
        let payload_len = header_size + snapshot_size + (bid_count + ask_count) * level_size;

        assert!(
            buf.len() >= payload_len,
            "buffer too small: {} < {}",
            buf.len(),
            payload_len
        );

        let record_len = payload_len as u32;

        // Write BookEventHeader
        let header = BookEventHeader {
            schema_version: PROTOCOL_VERSION,
            record_len,
            endianness: 0,
            _pad0: 0,
            venue_id,
            market_id,
            stream_id: 0,
            ingest_ts_ns: timestamp_ns,
            exchange_ts_ns: timestamp_ns,
            seq: 0,
            native_seq: 0,
            event_type: BookEventType::Snapshot as u8,
            book_mode: BookMode::L2 as u8,
            flags: book_flags::ABSOLUTE,
            _pad1: 0,
        };

        // Safety: BookEventHeader is repr(C) with explicit padding
        let header_bytes = unsafe { struct_to_bytes(&header) };
        buf[0..header_size].copy_from_slice(header_bytes);

        // Write L2Snapshot
        let snapshot = L2Snapshot {
            bid_count: bid_count as u32,
            ask_count: ask_count as u32,
            price_scale,
            size_scale,
            _pad0: 0,
        };

        // Safety: L2Snapshot is repr(C) with explicit padding
        let snapshot_bytes = unsafe { struct_to_bytes(&snapshot) };
        let mut offset = header_size;
        buf[offset..offset + snapshot_size].copy_from_slice(snapshot_bytes);
        offset += snapshot_size;

        // Write bid levels
        for (price, size) in bids {
            let level = PriceLevelUpdate { price, size };
            // Safety: PriceLevelUpdate is repr(C) with no padding
            let level_bytes = unsafe { struct_to_bytes(&level) };
            buf[offset..offset + level_size].copy_from_slice(level_bytes);
            offset += level_size;
        }

        // Write ask levels
        for (price, size) in asks {
            let level = PriceLevelUpdate { price, size };
            // Safety: PriceLevelUpdate is repr(C) with no padding
            let level_bytes = unsafe { struct_to_bytes(&level) };
            buf[offset..offset + level_size].copy_from_slice(level_bytes);
            offset += level_size;
        }

        payload_len
    }

    /// Calculates the required buffer size for an L2 snapshot with the given level counts.
    pub fn l2_snapshot_size(bid_count: usize, ask_count: usize) -> usize {
        std::mem::size_of::<BookEventHeader>()
            + std::mem::size_of::<L2Snapshot>()
            + (bid_count + ask_count) * std::mem::size_of::<PriceLevelUpdate>()
    }
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

    #[test]
    fn l3_event_size() {
        assert_eq!(size_of::<L3Event>(), 64);
    }

    #[test]
    fn test_l2_snapshot_size_calculation() {
        // Empty book
        let size = serialization::l2_snapshot_size(0, 0);
        assert_eq!(size, 64 + 12); // header + snapshot

        // 5 bids, 3 asks
        let size = serialization::l2_snapshot_size(5, 3);
        assert_eq!(size, 64 + 12 + 8 * 16); // header + snapshot + 8 levels
    }

    #[test]
    fn test_write_l2_snapshot_empty_book() {
        // Use aligned buffer
        let mut buf = vec![0u8; 1024];

        let written = serialization::write_l2_snapshot(
            &mut buf,
            1,     // venue_id
            100,   // market_id
            1000000000, // timestamp_ns
            4,     // price_scale
            0,     // size_scale
            std::iter::empty(),
            std::iter::empty(),
        );

        assert_eq!(written, 64 + 12); // header + snapshot, no levels

        // Helper to read struct from buffer safely
        fn read_copy<T: Copy>(buf: &[u8], offset: usize) -> T {
            assert!(buf.len() >= offset + size_of::<T>());
            unsafe {
                let ptr = buf.as_ptr().add(offset) as *const T;
                std::ptr::read_unaligned(ptr)
            }
        }

        // Verify header fields
        let header: BookEventHeader = read_copy(&buf, 0);
        assert_eq!(header.schema_version, PROTOCOL_VERSION);
        assert_eq!(header.venue_id, 1);
        assert_eq!(header.market_id, 100);
        assert_eq!(header.event_type, BookEventType::Snapshot as u8);
        assert_eq!(header.book_mode, BookMode::L2 as u8);

        // Verify snapshot fields
        let snapshot: L2Snapshot = read_copy(&buf, size_of::<BookEventHeader>());
        assert_eq!(snapshot.bid_count, 0);
        assert_eq!(snapshot.ask_count, 0);
        assert_eq!(snapshot.price_scale, 4);
        assert_eq!(snapshot.size_scale, 0);
    }

    #[test]
    fn test_write_l2_snapshot_with_levels() {
        let mut buf = vec![0u8; 1024];

        let bids = vec![(100, 50), (99, 60)];
        let asks = vec![(101, 40), (102, 30), (103, 20)];

        let written = serialization::write_l2_snapshot(
            &mut buf,
            1,     // venue_id
            100,   // market_id
            1000000000, // timestamp_ns
            4,     // price_scale
            2,     // size_scale
            bids.into_iter(),
            asks.into_iter(),
        );

        let expected_size = 64 + 12 + 5 * 16; // header + snapshot + 5 levels
        assert_eq!(written, expected_size);

        // Helper to read struct from buffer safely
        fn read_copy<T: Copy>(buf: &[u8], offset: usize) -> T {
            assert!(buf.len() >= offset + size_of::<T>());
            unsafe {
                let ptr = buf.as_ptr().add(offset) as *const T;
                std::ptr::read_unaligned(ptr)
            }
        }

        // Verify snapshot counts
        let snapshot: L2Snapshot = read_copy(&buf, size_of::<BookEventHeader>());
        assert_eq!(snapshot.bid_count, 2);
        assert_eq!(snapshot.ask_count, 3);
        assert_eq!(snapshot.price_scale, 4);
        assert_eq!(snapshot.size_scale, 2);

        // Verify first bid level
        let levels_offset = size_of::<BookEventHeader>() + size_of::<L2Snapshot>();
        let first_bid: PriceLevelUpdate = read_copy(&buf, levels_offset);
        assert_eq!(first_bid.price, 100);
        assert_eq!(first_bid.size, 50);

        // Verify first ask level (after 2 bid levels)
        let first_ask_offset = levels_offset + 2 * size_of::<PriceLevelUpdate>();
        let first_ask: PriceLevelUpdate = read_copy(&buf, first_ask_offset);
        assert_eq!(first_ask.price, 101);
        assert_eq!(first_ask.size, 40);
    }

    #[test]
    #[should_panic(expected = "buffer too small")]
    fn test_write_l2_snapshot_buffer_too_small() {
        let mut buf = vec![0u8; 10]; // Too small

        serialization::write_l2_snapshot(
            &mut buf,
            1, 100, 1000000000, 4, 0,
            vec![(100, 50)].into_iter(),
            std::iter::empty(),
        );
    }
}
