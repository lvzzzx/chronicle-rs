use std::path::{Path, PathBuf};

use chronicle::core::segment::DEFAULT_SEGMENT_SIZE;
use chronicle::ingest::market::market_id;
use chronicle::ingest::import::tardis_csv::import_tardis_incremental_book;
use clap::Parser;

#[derive(Parser)]
#[command(name = "chronicle-csv-import")]
#[command(about = "Import Tardis CSV incremental L2 book data directly into archive")]
struct Cli {
    /// Input CSV or CSV.GZ path
    #[arg(long)]
    input: PathBuf,

    /// Archive root path
    #[arg(long)]
    archive_root: PathBuf,

    /// Venue identifier (e.g. binance-futures)
    #[arg(long)]
    venue: String,

    /// Symbol identifier (e.g. BTCUSDT)
    #[arg(long)]
    symbol: String,

    /// Date partition (YYYY-MM-DD). If omitted, inferred from input path when possible.
    #[arg(long)]
    date: Option<String>,

    /// Stream name (default: book)
    #[arg(long, default_value = "book")]
    stream: String,

    /// Venue id to embed in BookEventHeader
    #[arg(long)]
    venue_id: u16,

    /// Market id to embed in BookEventHeader (default: hash of symbol)
    #[arg(long)]
    market_id: Option<u32>,

    /// Override price scale (decimal places) for all events
    #[arg(long)]
    price_scale: Option<u8>,

    /// Override size scale (decimal places) for all events
    #[arg(long)]
    size_scale: Option<u8>,

    /// Segment size in bytes
    #[arg(long, default_value_t = DEFAULT_SEGMENT_SIZE)]
    segment_size: usize,

    /// Overwrite ledger and import even if already imported
    #[arg(long)]
    force: bool,

    /// Compress archived segments after import
    #[arg(long)]
    compress: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let date = cli
        .date
        .or_else(|| infer_date_from_path(&cli.input))
        .ok_or("date is required or must be inferable from input path")?;
    let market_id = cli.market_id.unwrap_or_else(|| market_id(&cli.symbol));

    let stats = import_tardis_incremental_book(
        &cli.input,
        &cli.archive_root,
        &cli.venue,
        &cli.symbol,
        &date,
        &cli.stream,
        cli.venue_id,
        market_id,
        cli.segment_size,
        cli.price_scale,
        cli.size_scale,
        cli.force,
        cli.compress,
    )?;

    println!(
        "imported rows={} groups={} segments={} min_ts_ns={} max_ts_ns={}",
        stats.rows, stats.groups, stats.segments, stats.min_ts_ns, stats.max_ts_ns
    );

    Ok(())
}

fn infer_date_from_path(path: &Path) -> Option<String> {
    for component in path.components() {
        let value = component.as_os_str().to_string_lossy();
        if let Some(date) = value.strip_prefix("date=") {
            if is_date(date) {
                return Some(date.to_string());
            }
        }
    }

    let file_name = path.file_name()?.to_string_lossy();
    for part in file_name.split('_') {
        if is_date(part) {
            return Some(part.to_string());
        }
    }

    None
}

fn is_date(value: &str) -> bool {
    if value.len() != 10 {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    for (idx, byte) in bytes.iter().enumerate() {
        if idx == 4 || idx == 7 {
            continue;
        }
        if !byte.is_ascii_digit() {
            return false;
        }
    }
    true
}
