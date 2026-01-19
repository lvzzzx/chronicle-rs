use chronicle_core::Queue;
use std::path::PathBuf;
use std::time::Duration;
use clap::Parser;

// We need to redefine the struct here or expose it as a library.
// For now, I will copy the struct definition since I cannot easily depend on the binary crate's modules
// unless I refactor it to be a lib + bin.
// Given the simplicity, I will copy it for the example.

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct BookTicker {
    pub timestamp_ms: u64,
    pub bid_price: f64,
    pub bid_qty: f64,
    pub ask_price: f64,
    pub ask_qty: f64,
    pub symbol_hash: u64,
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the queue directory
    #[arg(short, long)]
    queue: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    println!("Reading from queue: {}", args.queue.display());

    let mut reader = Queue::open_subscriber(&args.queue, "cli_reader")?;
    println!("Reader connected. Waiting for messages...");

    loop {
        let _ = reader.wait(Some(Duration::from_millis(100)));
        while let Ok(Some(msg)) = reader.next() {
            if msg.type_id == 100 { // BookTicker
                let data = msg.payload;
                if data.len() == std::mem::size_of::<BookTicker>() {
                    let ticker = unsafe { &*(data.as_ptr() as *const BookTicker) };
                    println!(
                        "[{}] Hash:{} Bid:{}@{} Ask:{}@{}",
                        ticker.timestamp_ms,
                        ticker.symbol_hash,
                        ticker.bid_price,
                        ticker.bid_qty,
                        ticker.ask_price,
                        ticker.ask_qty
                    );
                } else {
                    eprintln!("Invalid payload size: {}", data.len());
                }
            }
        }
    }
}
