use anyhow::Result;
use chronicle_core::{Queue, WaitStrategy};
use chronicle_feed_binance::market::{DepthHeader, MarketMessageType, PriceLevel};
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
    bids: BTreeMap<u64, f64>, // Price(as u64 representation) -> Qty
    asks: BTreeMap<u64, f64>,
}

impl OrderBook {
    fn new() -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
        }
    }

    fn apply_snapshot(&mut self, bids: &[PriceLevel], asks: &[PriceLevel]) {
        self.bids.clear();
        self.asks.clear();
        for p in bids {
            self.bids.insert(to_ordered(p.price), p.qty);
        }
        for p in asks {
            self.asks.insert(to_ordered(p.price), p.qty);
        }
    }

    fn apply_update(&mut self, bids: &[PriceLevel], asks: &[PriceLevel]) {
        for p in bids {
            let key = to_ordered(p.price);
            if p.qty == 0.0 {
                self.bids.remove(&key);
            } else {
                self.bids.insert(key, p.qty);
            }
        }
        for p in asks {
            let key = to_ordered(p.price);
            if p.qty == 0.0 {
                self.asks.remove(&key);
            } else {
                self.asks.insert(key, p.qty);
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
                
                if type_id == MarketMessageType::OrderBookSnapshot as u16 || type_id == MarketMessageType::DepthUpdate as u16 {
                    unsafe {
                        // 1. Read Header
                        if payload.len() < std::mem::size_of::<DepthHeader>() {
                            continue;
                        }
                        let header_ptr = payload.as_ptr() as *const DepthHeader;
                        let header = &*header_ptr;
                        
                        let mut offset = std::mem::size_of::<DepthHeader>();
                        
                        // 2. Read Bids
                        let bids_size = header.bid_count as usize * std::mem::size_of::<PriceLevel>();
                        if payload.len() < offset + bids_size {
                            continue;
                        }
                        let bids_ptr = payload.as_ptr().add(offset) as *const PriceLevel;
                        let bids = std::slice::from_raw_parts(bids_ptr, header.bid_count as usize);
                        offset += bids_size;

                        // 3. Read Asks
                        let asks_size = header.ask_count as usize * std::mem::size_of::<PriceLevel>();
                        if payload.len() < offset + asks_size {
                            continue;
                        }
                        let asks_ptr = payload.as_ptr().add(offset) as *const PriceLevel;
                        let asks = std::slice::from_raw_parts(asks_ptr, header.ask_count as usize);

                        // 4. Apply
                        if type_id == MarketMessageType::OrderBookSnapshot as u16 {
                            println!("Received Snapshot (ID: {})", header.final_update_id);
                            book.apply_snapshot(bids, asks);
                            initialized = true;
                        } else if initialized {
                            // Depth Update - only apply if we have a base snapshot
                            book.apply_update(bids, asks);
                        } else {
                            // Skipping update because we haven't seen a snapshot yet
                            // In a real app, you might log this periodically
                        }
                        
                        if initialized {
                            book.print_top_5();
                        }
                    }
                } else if type_id == MarketMessageType::Trade as u16 {
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
