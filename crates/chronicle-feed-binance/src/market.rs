use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum MarketMessageType {
    BookTicker = 100,
    Trade = 101,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[repr(C)]
pub struct BookTicker {
    pub timestamp_ms: u64,
    pub bid_price: f64,
    pub bid_qty: f64,
    pub ask_price: f64,
    pub ask_qty: f64,
    pub symbol_hash: u64, // FNV-1a hash of the symbol string
}

impl BookTicker {
    pub fn new(
        timestamp_ms: u64,
        bid_price: f64,
        bid_qty: f64,
        ask_price: f64,
        ask_qty: f64,
        symbol: &str,
    ) -> Self {
        Self {
            timestamp_ms,
            bid_price,
            bid_qty,
            ask_price,
            ask_qty,
            symbol_hash: fxhash::hash64(symbol),
        }
    }
}

// Simple FNV-1a style hash for symbol strings to keep the struct fixed-size
mod fxhash {
    pub fn hash64(text: &str) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in text.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x1099511628211904);
        }
        hash
    }
}
