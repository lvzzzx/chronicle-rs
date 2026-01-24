use crate::protocol::{BookTicker, Trade};

pub trait Appendable {
    fn size(&self) -> usize;
    // Write content to the provided buffer. Buffer length is guaranteed to be self.size().
    fn write_to(&self, buf: &mut [u8]);
    fn timestamp_ns(&self) -> u64;
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

    fn timestamp_ns(&self) -> u64 {
        self.timestamp_ns
    }
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

    fn timestamp_ns(&self) -> u64 {
        self.timestamp_ns
    }
}

pub fn market_id(symbol: &str) -> u32 {
    let h = fxhash::hash64(symbol);
    ((h >> 32) as u32) ^ (h as u32)
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
