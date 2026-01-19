use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum MarketMessageType {
    BookTicker = 100,
    #[allow(dead_code)]
    Trade = 101,
    DepthUpdate = 102,
    OrderBookSnapshot = 103,
}

pub trait Appendable {
    fn size(&self) -> usize;
    // Write content to the provided buffer. Buffer length is guaranteed to be self.size()
    fn write_to(&self, buf: &mut [u8]);
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[repr(C)]
pub struct PriceLevel {
    pub price: f64,
    pub qty: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[repr(C)]
pub struct DepthHeader {
    pub timestamp_ms: u64,
    pub first_update_id: u64,
    pub final_update_id: u64,
    pub symbol_hash: u64,
    pub bid_count: u32,
    pub ask_count: u32,
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

impl Appendable for BookTicker {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
    fn write_to(&self, buf: &mut [u8]) {
        let ptr = self as *const Self as *const u8;
        unsafe {
            let src = std::slice::from_raw_parts(ptr, self.size());
            buf.copy_from_slice(src);
        }
    }
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[repr(C)]
pub struct Trade {
    pub timestamp_ms: u64, // Trade time (T)
    pub price: f64,
    pub qty: f64,
    pub trade_id: u64,
    pub buyer_order_id: u64,
    pub seller_order_id: u64,
    pub is_buyer_maker: bool,
    pub symbol_hash: u64,
}

impl Appendable for Trade {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
    fn write_to(&self, buf: &mut [u8]) {
        let ptr = self as *const Self as *const u8;
        unsafe {
            let src = std::slice::from_raw_parts(ptr, self.size());
            buf.copy_from_slice(src);
        }
    }
}

impl Trade {
    pub fn new(
        timestamp_ms: u64,
        price: f64,
        qty: f64,
        trade_id: u64,
        buyer_order_id: u64,
        seller_order_id: u64,
        is_buyer_maker: bool,
        symbol: &str,
    ) -> Self {
        Self {
            timestamp_ms,
            price,
            qty,
            trade_id,
            buyer_order_id,
            seller_order_id,
            is_buyer_maker,
            symbol_hash: fxhash::hash64(symbol),
        }
    }
}

// Simple FNV-1a style hash for symbol strings to keep the struct fixed-size
pub mod fxhash {
    pub fn hash64(text: &str) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in text.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x1099511628211904);
        }
        hash
    }
}
