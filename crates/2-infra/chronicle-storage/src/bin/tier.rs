use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use chronicle_storage::{TierConfig, TierManager};

#[derive(Parser)]
#[command(name = "chronicle-storage-tier")]
#[command(about = "Tier manager for Chronicle storage")]
struct Cli {
    /// Root path for v1 storage layout or queue directory
    #[arg(long)]
    root: PathBuf,

    /// Zstd block size in bytes (default 1 MiB)
    #[arg(long, default_value_t = 1_048_576)]
    block_size: usize,

    /// Age threshold in seconds before moving warm -> cold
    #[arg(long, default_value_t = 0)]
    cold_after_secs: u64,

    /// Age threshold in seconds before moving cold -> remote
    #[arg(long, default_value_t = 0)]
    remote_after_secs: u64,

    /// Remote root path for cold storage (optional)
    #[arg(long)]
    remote_root: Option<PathBuf>,

    /// Run a single pass and exit
    #[arg(long)]
    once: bool,

    /// Interval in seconds between scans (ignored with --once)
    #[arg(long, default_value_t = 60)]
    interval_secs: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let mut config = TierConfig::new(cli.root);
    config.block_size = cli.block_size;
    config.cold_after = Duration::from_secs(cli.cold_after_secs);
    config.remote_after = Duration::from_secs(cli.remote_after_secs);
    config.remote_root = cli.remote_root;

    let manager = TierManager::new(config);

    if cli.once {
        manager.run_once()?;
        return Ok(());
    }

    loop {
        manager.run_once()?;
        std::thread::sleep(Duration::from_secs(cli.interval_secs));
    }
}
