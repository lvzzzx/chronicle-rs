use std::path::PathBuf;
use std::time::Duration;

use chronicle::storage::{RawArchiver, RawArchiverConfig};
use clap::Parser;

#[derive(Parser)]
#[command(name = "chronicle-raw-archiver")]
#[command(about = "Archive sealed raw IPC segments into the raw archive layout")]
struct Cli {
    /// Source raw queue directory (streams/raw/<venue>/queue)
    #[arg(long)]
    source: PathBuf,

    /// Archive root path
    #[arg(long)]
    archive_root: PathBuf,

    /// Venue identifier (e.g. binance-perp)
    #[arg(long)]
    venue: String,

    /// Enable zstd compression for archived segments
    #[arg(long)]
    compress: bool,

    /// Keep .q files after compression (default: delete after compress)
    #[arg(long)]
    retain_q: bool,

    /// Zstd block size in bytes (default 1 MiB)
    #[arg(long, default_value_t = 1_048_576)]
    block_size: usize,

    /// Reader name for retention tracking
    #[arg(long, default_value = "raw-archiver")]
    reader_name: String,

    /// Run a single pass and exit
    #[arg(long)]
    once: bool,

    /// Poll interval in milliseconds between scans (ignored with --once)
    #[arg(long, default_value_t = 1000)]
    poll_ms: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let mut config = RawArchiverConfig::new(cli.source, cli.archive_root, cli.venue);
    config.compress = cli.compress;
    config.retain_q = cli.retain_q;
    config.block_size = cli.block_size;
    config.reader_name = cli.reader_name;
    config.poll_interval = Duration::from_millis(cli.poll_ms);

    let mut archiver = RawArchiver::new(config);
    if cli.once {
        archiver.run_once()?;
        return Ok(());
    }

    archiver.run_loop()?;
    Ok(())
}
