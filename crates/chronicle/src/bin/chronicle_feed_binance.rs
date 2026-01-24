use anyhow::{Context, Result};
use clap::Parser;
use log::info;
use std::path::PathBuf;

use chronicle::core::Queue;
use chronicle::feed_binance::binance;

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

    /// CPU core to pin the process to
    #[arg(long)]
    core_id: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    if let Some(core_id) = args.core_id {
        let core_ids = core_affinity::get_core_ids().context("Failed to get core IDs")?;
        if core_id < core_ids.len() {
            info!("Pinning process to core {}", core_id);
            core_affinity::set_for_current(core_ids[core_id]);
        } else {
            anyhow::bail!("Core ID {} out of range ({} cores available)", core_id, core_ids.len());
        }
    }

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
    let config = chronicle::core::writer::WriterConfig::ultra_low_latency();
    let mut writer = Queue::open_publisher_with_config(&queue_path, config)
        .context("Failed to open queue writer")?;

    let symbols: Vec<String> = args.symbols.split(',')
        .map(|s| s.trim().to_string())
        .collect();

    let feed = binance::BinanceFeed::new(&symbols);

    // Start the feed
    // We pass a closure that writes to the queue using zero-copy append_in_place
    feed.run(move |type_id, event| {
        let payload_len = event.size();
        let timestamp_ns = event.timestamp_ns();
        writer
            .append_in_place_with_timestamp(type_id, payload_len, timestamp_ns, |buf| {
                event.write_to(buf);
                Ok(())
            })
            .map_err(|e| anyhow::anyhow!(e))
    })
    .await?;

    Ok(())
}
