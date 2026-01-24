use anyhow::Result;
use chronicle::core::Queue;
use chronicle::protocol::{BookEventHeader, BookEventType, BookMode, L2Diff, L2Snapshot, PriceLevelUpdate, TypeId};
use clap::Parser;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = "./data/market_data/binance_spot")]
    queue: PathBuf,
}

struct OrderBook {
    bids: BTreeMap<u64, f64>, // Price (as u64 bits) -> Qty
    asks: BTreeMap<u64, f64>,
}

impl OrderBook {
    fn new() -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
        }
    }

    fn apply_snapshot(&mut self, bids: &[PriceLevelUpdate], asks: &[PriceLevelUpdate], price_scale: u8, size_scale: u8) {
        self.bids.clear();
        self.asks.clear();
        for p in bids {
            self.bids.insert(to_ordered(to_f64(p.price, price_scale)), to_f64(p.size, size_scale));
        }
        for p in asks {
            self.asks.insert(to_ordered(to_f64(p.price, price_scale)), to_f64(p.size, size_scale));
        }
    }

    fn apply_update(&mut self, bids: &[PriceLevelUpdate], asks: &[PriceLevelUpdate], price_scale: u8, size_scale: u8) {
        for p in bids {
            let price = to_f64(p.price, price_scale);
            let qty = to_f64(p.size, size_scale);
            let key = to_ordered(price);
            if qty == 0.0 {
                self.bids.remove(&key);
            } else {
                self.bids.insert(key, qty);
            }
        }
        for p in asks {
            let price = to_f64(p.price, price_scale);
            let qty = to_f64(p.size, size_scale);
            let key = to_ordered(price);
            if qty == 0.0 {
                self.asks.remove(&key);
            } else {
                self.asks.insert(key, qty);
            }
        }
    }

    fn print_top_5(&self) {
        println!("--- Order Book ---");
        println!("ASKS:");
        for (p, q) in self.asks.iter().take(5) {
            println!("  {:.2} x {:.4}", from_ordered(*p), q);
        }
        println!("BIDS:");
        for (p, q) in self.bids.iter().rev().take(5) {
            println!("  {:.2} x {:.4}", from_ordered(*p), q);
        }
        println!("------------------");
    }
}

// Float keys in BTreeMap hack (for example purposes only)
fn to_ordered(f: f64) -> u64 {
    f.to_bits()
}

fn from_ordered(u: u64) -> f64 {
    f64::from_bits(u)
}

fn to_f64(value: u64, scale: u8) -> f64 {
    if scale == 0 {
        return value as f64;
    }
    let denom = 10_f64.powi(scale as i32);
    (value as f64) / denom
}

fn main() -> Result<()> {
    let args = Args::parse();
    // Use a fixed reader name "book-reconstructor" to resume from where we left off
    let mut reader = Queue::open_subscriber(&args.queue, "book-reconstructor")?;
    let mut book = OrderBook::new();
    let mut initialized = false;

    println!("Listening for updates on {}", args.queue.display());

    loop {
        // Try to read the next message
        match reader.next()? {
            Some(msg) => {
                let type_id = msg.type_id;
                let payload = msg.payload;

                // Safety: We assume the producer follows the schema.
                // In prod, you'd validate bounds.
                
                if type_id == TypeId::BookEvent.as_u16() {
                    unsafe {
                        // 1. Read Header
                        if payload.len() < std::mem::size_of::<BookEventHeader>() {
                            continue;
                        }
                        let header_ptr = payload.as_ptr() as *const BookEventHeader;
                        let header = &*header_ptr;

                        if header.book_mode != BookMode::L2 as u8 {
                            continue;
                        }

                        let mut offset = std::mem::size_of::<BookEventHeader>();

                        match header.event_type {
                            x if x == BookEventType::Snapshot as u8 => {
                                if payload.len() < offset + std::mem::size_of::<L2Snapshot>() {
                                    continue;
                                }
                                let snap_ptr = payload.as_ptr().add(offset) as *const L2Snapshot;
                                let snap = &*snap_ptr;
                                offset += std::mem::size_of::<L2Snapshot>();

                                let total = snap.bid_count as usize + snap.ask_count as usize;
                                let levels_size = total * std::mem::size_of::<PriceLevelUpdate>();
                                if payload.len() < offset + levels_size {
                                    continue;
                                }
                                let levels_ptr = payload.as_ptr().add(offset) as *const PriceLevelUpdate;
                                let levels = std::slice::from_raw_parts(levels_ptr, total);
                                let (bids, asks) = levels.split_at(snap.bid_count as usize);

                                println!("Received Snapshot (seq: {})", header.native_seq);
                                book.apply_snapshot(bids, asks, snap.price_scale, snap.size_scale);
                                initialized = true;
                            }
                            x if x == BookEventType::Diff as u8 => {
                                if payload.len() < offset + std::mem::size_of::<L2Diff>() {
                                    continue;
                                }
                                let diff_ptr = payload.as_ptr().add(offset) as *const L2Diff;
                                let diff = &*diff_ptr;
                                offset += std::mem::size_of::<L2Diff>();

                                let total = diff.bid_count as usize + diff.ask_count as usize;
                                let levels_size = total * std::mem::size_of::<PriceLevelUpdate>();
                                if payload.len() < offset + levels_size {
                                    continue;
                                }
                                let levels_ptr = payload.as_ptr().add(offset) as *const PriceLevelUpdate;
                                let levels = std::slice::from_raw_parts(levels_ptr, total);
                                let (bids, asks) = levels.split_at(diff.bid_count as usize);

                                if initialized {
                                    book.apply_update(bids, asks, diff.price_scale, diff.size_scale);
                                }
                            }
                            _ => {}
                        }

                        if initialized {
                            book.print_top_5();
                        }
                    }
                } else if type_id == TypeId::Trade.as_u16() {
                    // ... handle trade
                }
            }
            None => {
                // No new message, wait for one
                reader.wait(None)?;
            }
        }
    }
}
