use chronicle_core::Queue;
use chronicle_protocol::{BookTicker, TypeId};
use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;

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
            if msg.type_id == TypeId::BookTicker.as_u16() {
                let data = msg.payload;
                if data.len() == std::mem::size_of::<BookTicker>() {
                    let ticker = unsafe { &*(data.as_ptr() as *const BookTicker) };
                    println!(
                        "[{}] Hash:{} Bid:{}@{} Ask:{}@{}",
                        ticker.timestamp_ns,
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
