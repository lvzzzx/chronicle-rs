use std::path::{Path, PathBuf};

use chronicle::core::segment::DEFAULT_SEGMENT_SIZE;
use chronicle::import::szse_l3_csv::import_szse_l3_channel;
use clap::Parser;

#[derive(Parser)]
#[command(name = "chronicle-szse-import")]
#[command(about = "Import SZSE L3 channel CSV into archive")]
struct Cli {
    /// Input channel CSV path (e.g. channel_2011.csv)
    #[arg(long)]
    input: PathBuf,

    /// Archive root path
    #[arg(long)]
    archive_root: PathBuf,

    /// Date partition (YYYY-MM-DD). If omitted, inferred from input path when possible.
    #[arg(long)]
    date: Option<String>,

    /// Channel number (optional, used for validation)
    #[arg(long)]
    channel: Option<u32>,

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

    let stats = import_szse_l3_channel(
        &cli.input,
        &cli.archive_root,
        &date,
        cli.channel,
        cli.segment_size,
        cli.force,
        cli.compress,
    )?;

    println!(
        "imported rows={} segments={} min_ts_ns={} max_ts_ns={} channel={}",
        stats.rows, stats.segments, stats.min_ts_ns, stats.max_ts_ns, stats.channel
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
        if let Some(date) = yyyymmdd_to_date(&value) {
            return Some(date);
        }
    }

    let file_name = path.file_name()?.to_string_lossy();
    for part in file_name.split('_') {
        if let Some(date) = yyyymmdd_to_date(part) {
            return Some(date);
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

fn yyyymmdd_to_date(value: &str) -> Option<String> {
    if value.len() != 8 || !value.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!(
        "{}-{}-{}",
        &value[0..4],
        &value[4..6],
        &value[6..8]
    ))
}
