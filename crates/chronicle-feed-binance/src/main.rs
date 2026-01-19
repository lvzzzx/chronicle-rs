use anyhow::{Context, Result};
use clap::Parser;
use log::info;
use std::path::PathBuf;

use chronicle_core::Queue;

mod binance;
mod market;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Chronicle Bus Root path (optional, overrides output-path if used with standard layout)
    #[arg(long)]
    bus_root: Option<PathBuf>,

    /// Direct path to the output queue
    #[arg(short, long)]
    queue: Option<PathBuf>,

    /// Comma-separated list of symbols to subscribe (e.g., btcusdt,ethusdt)
    #[arg(short, long, default_value = "btcusdt,ethusdt,solusdt")]
    symbols: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    let queue_path = if let Some(path) = args.queue {
        path
    } else if let Some(root) = args.bus_root {
        root.join("market_data").join("binance_spot")
    } else {
        PathBuf::from("./data/market_data/binance_spot")
    };

    info!("Starting Binance Feed");
    info!("Output Queue: {}", queue_path.display());
    info!("Symbols: {}", args.symbols);

    // Initialize Chronicle Writer
    // Use Ultra-Low-Latency config
    let config = chronicle_core::writer::WriterConfig::ultra_low_latency();
    let mut writer = Queue::open_publisher_with_config(&queue_path, config)
        .context("Failed to open queue writer")?;

    let symbols: Vec<String> = args.symbols.split(',')
        .map(|s| s.trim().to_string())
        .collect();

    let feed = binance::BinanceFeed::new(&symbols);

    // Start the feed
    // We pass a closure that writes to the queue
    feed.run(move |type_id, data| {
        // This closure is called for every market event
        // Note: Chronicle writer is not thread-safe, but run() is currently single-threaded in its processing loop
        // If we parallelize, we need a mutex or multiple writers.
        // Since we are inside a single-threaded async loop (conceptually), we can just call append.
        // However, 'writer' needs to be mutable.
        writer.append(type_id, data).map_err(|e| anyhow::anyhow!(e))
    }).await?;

    Ok(())
}