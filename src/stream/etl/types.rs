//! Type abstractions for ETL module to decouple from replay internals.
//!
//! This module defines traits that abstract over replay module types, allowing
//! features and extractors to work with different book implementations and event
//! sources without tight coupling to replay module internals.

use anyhow::Result;

/// Abstraction over order book state for feature computation.
///
/// This trait provides read-only access to an L2 order book's bid and ask sides,
/// allowing features to compute metrics without depending on the concrete L2Book type.
pub trait OrderBook {
    /// Returns the best bid price and size, if available.
    fn best_bid(&self) -> Option<(u64, u64)>;

    /// Returns the best ask price and size, if available.
    fn best_ask(&self) -> Option<(u64, u64)>;

    /// Returns an iterator over bid levels (price, size) in descending price order.
    fn bid_levels(&self) -> Box<dyn Iterator<Item = (u64, u64)> + '_>;

    /// Returns an iterator over ask levels (price, size) in ascending price order.
    fn ask_levels(&self) -> Box<dyn Iterator<Item = (u64, u64)> + '_>;

    /// Returns the (price_scale, size_scale) for this book.
    fn scales(&self) -> (u8, u8);
}

/// Abstraction over book event headers.
///
/// This trait provides access to metadata from book events without exposing
/// the underlying protocol header structure.
pub trait EventHeader {
    /// Venue/exchange identifier.
    fn venue_id(&self) -> u16;

    /// Market/symbol identifier.
    fn market_id(&self) -> u32;

    /// Exchange timestamp in nanoseconds.
    fn exchange_ts_ns(&self) -> u64;

    /// Ingest timestamp in nanoseconds.
    fn ingest_ts_ns(&self) -> u64;

    /// Event type code (snapshot, diff, reset, etc.).
    fn event_type_code(&self) -> u8;

    /// Sequence number.
    fn seq(&self) -> u64;
}

/// Abstraction over book update events.
///
/// This trait provides access to book events and their classification
/// without exposing the underlying BookEvent/BookEventPayload enum.
pub trait BookUpdate {
    /// Returns the event header.
    fn header(&self) -> &dyn EventHeader;

    /// Returns true if this is a snapshot event.
    fn is_snapshot(&self) -> bool;

    /// Returns true if this is a diff (incremental update) event.
    fn is_diff(&self) -> bool;

    /// Returns true if this is a reset event.
    fn is_reset(&self) -> bool;

    /// Returns true if this is a heartbeat event.
    fn is_heartbeat(&self) -> bool;

    /// Returns true if this event type is supported.
    fn is_supported(&self) -> bool;
}

/// Abstraction over replay messages.
///
/// This trait provides access to messages from a replay stream without
/// coupling to the concrete ReplayMessage type.
pub trait Message {
    /// Returns the current book state.
    fn book(&self) -> &dyn OrderBook;

    /// Returns the book update event, if this message contains one.
    fn update(&self) -> Option<&dyn BookUpdate>;

    /// Returns the venue ID if this is a book event.
    fn venue_id(&self) -> Option<u16> {
        self.update().map(|u| u.header().venue_id())
    }

    /// Returns the market ID if this is a book event.
    fn market_id(&self) -> Option<u32> {
        self.update().map(|u| u.header().market_id())
    }

    /// Returns the exchange timestamp if this is a book event.
    fn exchange_ts_ns(&self) -> Option<u64> {
        self.update().map(|u| u.header().exchange_ts_ns())
    }

    /// Returns the ingest timestamp if this is a book event.
    fn ingest_ts_ns(&self) -> Option<u64> {
        self.update().map(|u| u.header().ingest_ts_ns())
    }
}

/// Abstraction over message sources for ETL processing.
///
/// This trait allows ETL components to iterate over messages from different
/// sources (live streams, archives, tests) without coupling to LiveReplayEngine.
pub trait MessageSource {
    /// Returns the next message, or None if the stream is exhausted.
    fn next_message(&mut self) -> Result<Option<Box<dyn Message + '_>>>;
}

// ============================================================================
// Adapter implementations for replay module types
// ============================================================================

use crate::protocol::BookEventHeader;
use crate::stream::replay::{BookEvent, BookEventPayload, L2Book, ReplayMessage};

/// Adapter: Implements OrderBook for replay::L2Book
impl OrderBook for L2Book {
    fn best_bid(&self) -> Option<(u64, u64)> {
        self.bids().iter().next_back().map(|(&p, &s)| (p, s))
    }

    fn best_ask(&self) -> Option<(u64, u64)> {
        self.asks().iter().next().map(|(&p, &s)| (p, s))
    }

    fn bid_levels(&self) -> Box<dyn Iterator<Item = (u64, u64)> + '_> {
        Box::new(self.bids().iter().rev().map(|(&p, &s)| (p, s)))
    }

    fn ask_levels(&self) -> Box<dyn Iterator<Item = (u64, u64)> + '_> {
        Box::new(self.asks().iter().map(|(&p, &s)| (p, s)))
    }

    fn scales(&self) -> (u8, u8) {
        self.scales()
    }
}

/// Adapter: Implements EventHeader for protocol::BookEventHeader
impl EventHeader for BookEventHeader {
    fn venue_id(&self) -> u16 {
        self.venue_id
    }

    fn market_id(&self) -> u32 {
        self.market_id
    }

    fn exchange_ts_ns(&self) -> u64 {
        self.exchange_ts_ns
    }

    fn ingest_ts_ns(&self) -> u64 {
        self.ingest_ts_ns
    }

    fn event_type_code(&self) -> u8 {
        self.event_type
    }

    fn seq(&self) -> u64 {
        self.seq
    }
}

/// Adapter: Implements BookUpdate for replay::BookEvent
impl<'a> BookUpdate for BookEvent<'a> {
    fn header(&self) -> &dyn EventHeader {
        &self.header
    }

    fn is_snapshot(&self) -> bool {
        matches!(self.payload, BookEventPayload::Snapshot { .. })
    }

    fn is_diff(&self) -> bool {
        matches!(self.payload, BookEventPayload::Diff { .. })
    }

    fn is_reset(&self) -> bool {
        matches!(self.payload, BookEventPayload::Reset)
    }

    fn is_heartbeat(&self) -> bool {
        matches!(self.payload, BookEventPayload::Heartbeat)
    }

    fn is_supported(&self) -> bool {
        !matches!(self.payload, BookEventPayload::Unsupported)
    }
}

/// Adapter: Implements Message for replay::ReplayMessage
impl<'a> Message for ReplayMessage<'a> {
    fn book(&self) -> &dyn OrderBook {
        self.book
    }

    fn update(&self) -> Option<&dyn BookUpdate> {
        self.book_event
            .as_ref()
            .map(|event| event as &dyn BookUpdate)
    }
}

/// Adapter: Implements MessageSource for ReplayEngine
use crate::stream::replay::ReplayEngine;
use crate::stream::StreamReader;

impl<R: StreamReader> MessageSource for ReplayEngine<R> {
    fn next_message(&mut self) -> Result<Option<Box<dyn Message + '_>>> {
        match self.next_message()? {
            Some(msg) => Ok(Some(Box::new(msg))),
            None => Ok(None),
        }
    }
}

// ============================================================================
// Test utilities
// ============================================================================

#[cfg(test)]
pub mod test_utils {
    use super::*;
    use std::collections::BTreeMap;

    /// Mock order book for testing features in isolation.
    #[derive(Debug, Clone)]
    pub struct MockOrderBook {
        pub bids: BTreeMap<u64, u64>,
        pub asks: BTreeMap<u64, u64>,
        pub price_scale: u8,
        pub size_scale: u8,
    }

    impl MockOrderBook {
        pub fn new() -> Self {
            Self {
                bids: BTreeMap::new(),
                asks: BTreeMap::new(),
                price_scale: 4,
                size_scale: 0,
            }
        }

        pub fn with_scales(price_scale: u8, size_scale: u8) -> Self {
            Self {
                bids: BTreeMap::new(),
                asks: BTreeMap::new(),
                price_scale,
                size_scale,
            }
        }

        pub fn add_bid(&mut self, price: u64, size: u64) {
            self.bids.insert(price, size);
        }

        pub fn add_ask(&mut self, price: u64, size: u64) {
            self.asks.insert(price, size);
        }
    }

    impl Default for MockOrderBook {
        fn default() -> Self {
            Self::new()
        }
    }

    impl OrderBook for MockOrderBook {
        fn best_bid(&self) -> Option<(u64, u64)> {
            self.bids.iter().next_back().map(|(&p, &s)| (p, s))
        }

        fn best_ask(&self) -> Option<(u64, u64)> {
            self.asks.iter().next().map(|(&p, &s)| (p, s))
        }

        fn bid_levels(&self) -> Box<dyn Iterator<Item = (u64, u64)> + '_> {
            Box::new(self.bids.iter().rev().map(|(&p, &s)| (p, s)))
        }

        fn ask_levels(&self) -> Box<dyn Iterator<Item = (u64, u64)> + '_> {
            Box::new(self.asks.iter().map(|(&p, &s)| (p, s)))
        }

        fn scales(&self) -> (u8, u8) {
            (self.price_scale, self.size_scale)
        }
    }

    /// Mock event header for testing.
    #[derive(Debug, Clone)]
    pub struct MockEventHeader {
        pub venue_id: u16,
        pub market_id: u32,
        pub exchange_ts_ns: u64,
        pub ingest_ts_ns: u64,
        pub event_type_code: u8,
        pub seq: u64,
    }

    impl MockEventHeader {
        pub fn new() -> Self {
            Self {
                venue_id: 0,
                market_id: 0,
                exchange_ts_ns: 0,
                ingest_ts_ns: 0,
                event_type_code: 0,
                seq: 0,
            }
        }

        pub fn with_timestamps(exchange_ts_ns: u64, ingest_ts_ns: u64) -> Self {
            Self {
                venue_id: 0,
                market_id: 0,
                exchange_ts_ns,
                ingest_ts_ns,
                event_type_code: 0,
                seq: 0,
            }
        }
    }

    impl Default for MockEventHeader {
        fn default() -> Self {
            Self::new()
        }
    }

    impl EventHeader for MockEventHeader {
        fn venue_id(&self) -> u16 {
            self.venue_id
        }

        fn market_id(&self) -> u32 {
            self.market_id
        }

        fn exchange_ts_ns(&self) -> u64 {
            self.exchange_ts_ns
        }

        fn ingest_ts_ns(&self) -> u64 {
            self.ingest_ts_ns
        }

        fn event_type_code(&self) -> u8 {
            self.event_type_code
        }

        fn seq(&self) -> u64 {
            self.seq
        }
    }

    /// Mock book update for testing.
    #[derive(Debug)]
    pub struct MockBookUpdate {
        pub header: MockEventHeader,
        pub is_snapshot: bool,
        pub is_diff: bool,
        pub is_reset: bool,
        pub is_heartbeat: bool,
    }

    impl MockBookUpdate {
        pub fn new(header: MockEventHeader) -> Self {
            Self {
                header,
                is_snapshot: false,
                is_diff: true,
                is_reset: false,
                is_heartbeat: false,
            }
        }

        pub fn snapshot(header: MockEventHeader) -> Self {
            Self {
                header,
                is_snapshot: true,
                is_diff: false,
                is_reset: false,
                is_heartbeat: false,
            }
        }
    }

    impl BookUpdate for MockBookUpdate {
        fn header(&self) -> &dyn EventHeader {
            &self.header
        }

        fn is_snapshot(&self) -> bool {
            self.is_snapshot
        }

        fn is_diff(&self) -> bool {
            self.is_diff
        }

        fn is_reset(&self) -> bool {
            self.is_reset
        }

        fn is_heartbeat(&self) -> bool {
            self.is_heartbeat
        }

        fn is_supported(&self) -> bool {
            true
        }
    }
}

