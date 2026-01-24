use anyhow::Result;
use clap::{Parser, Subcommand};
use chronicle_access::{StorageReader, StorageResolver};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "chronicle-access")]
#[command(about = "Tier-aware readers for Chronicle storage")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Read messages from a tiered storage path
    ExampleRead {
        /// Root path (used for all tiers if specific roots are omitted)
        #[arg(long)]
        root: PathBuf,

        /// Optional hot tier root (overrides --root for hot reads)
        #[arg(long)]
        hot_root: Option<PathBuf>,

        /// Optional warm tier root (overrides --root for warm reads)
        #[arg(long)]
        warm_root: Option<PathBuf>,

        /// Optional cold tier root (overrides --root for cold/remote reads)
        #[arg(long)]
        cold_root: Option<PathBuf>,

        /// Optional cache root for remote cold downloads
        #[arg(long)]
        cache_root: Option<PathBuf>,

        /// Venue identifier (directory name under v1)
        #[arg(long)]
        venue: String,

        /// Symbol code (directory name under v1)
        #[arg(long)]
        symbol: String,

        /// Date partition (YYYY-MM-DD)
        #[arg(long)]
        date: String,

        /// Stream name (default: book)
        #[arg(long, default_value = "book")]
        stream: String,

        /// Optional timestamp (ns) to seek before reading
        #[arg(long)]
        seek_ts_ns: Option<u64>,

        /// Optional max messages to print
        #[arg(long)]
        limit: Option<usize>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::ExampleRead {
            root,
            hot_root,
            warm_root,
            cold_root,
            cache_root,
            venue,
            symbol,
            date,
            stream,
            seek_ts_ns,
            limit,
        } => {
            let mut resolver = StorageResolver::with_roots(
                hot_root.or_else(|| Some(root.clone())),
                warm_root.or_else(|| Some(root.clone())),
                cold_root.or_else(|| Some(root.clone())),
            );
            if let Some(cache_root) = cache_root {
                resolver = resolver.with_cache_root(cache_root);
            }

            let mut reader = StorageReader::open(&resolver, &venue, &symbol, &date, &stream)?;
            println!("tier={:?}", reader.tier());

            if let Some(ts) = seek_ts_ns {
                let found = reader.seek_timestamp(ts)?;
                println!("seek_ts_ns={} found={}", ts, found);
            }

            let mut count = 0usize;
            while let Some(msg) = reader.next()? {
                println!(
                    "seq={} ts_ns={} type={} len={}",
                    msg.seq,
                    msg.timestamp_ns,
                    msg.type_id,
                    msg.payload.len()
                );
                count += 1;
                if limit.map_or(false, |max| count >= max) {
                    break;
                }
            }
        }
    }

    Ok(())
}
