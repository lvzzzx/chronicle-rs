use std::env;
use std::mem::size_of;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use chronicle::core::{ReaderConfig, StartMode};
use chronicle::protocol::{BookMode, L2Snapshot, PriceLevelUpdate};
use chronicle::replay::snapshot::{
    SnapshotMetadata, SnapshotPlanner, SnapshotPolicy, SnapshotRetention, SnapshotWriter,
    SNAPSHOT_VERSION,
};
use chronicle::replay::{ReplayEngine, ReplayUpdate};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return Ok(());
    }

    let config = parse_args(&args).map_err(anyhow::Error::msg)?;

    let reader_config = ReaderConfig {
        memlock: false,
        start_mode: config.start_mode,
    };
    let mut engine =
        ReplayEngine::open_with_config(&config.archive_path, "snapshotter", reader_config)?;

    let policy = SnapshotPolicy {
        min_interval: config.min_interval,
        min_records: config.min_records,
        min_bytes: config.min_bytes,
    };
    let mut planner = SnapshotPlanner::new(policy);
    let writer = SnapshotWriter::new(&config.archive_path, SnapshotRetention::default());

    let mut window_start_ingest: Option<u64> = None;
    let mut window_start_exchange: Option<u64> = None;

    println!(
        "snapshotter: archive={} venue={} market={} start={:?}",
        config.archive_path.display(),
        config.venue_id,
        config.market_id,
        config.start_mode
    );

    loop {
        engine.wait(None)?;
        let Some(message) = engine.next_message()? else {
            continue;
        };

        if let ReplayUpdate::Applied { .. } = message.update {
            if let Some(book_event) = message.book_event {
                if book_event.header.book_mode == BookMode::L2 as u8
                    && book_event.header.venue_id == config.venue_id
                    && book_event.header.market_id == config.market_id
                {
                    let ingest_ts = book_event.header.ingest_ts_ns;
                    let exchange_ts = book_event.header.exchange_ts_ns;
                    window_start_ingest.get_or_insert(ingest_ts);
                    window_start_exchange.get_or_insert(exchange_ts);

                    planner.observe(1, message.msg.payload.len() as u64);

                    if planner.should_snapshot(SystemTime::now()) {
                        let (snapshot, payload) = build_snapshot_payload(message.book);
                        let metadata = SnapshotMetadata {
                            schema_version: SNAPSHOT_VERSION,
                            endianness: 0,
                            book_mode: BookMode::L2 as u16,
                            venue_id: u32::from(config.venue_id),
                            market_id: config.market_id,
                            seq_num: message.msg.seq,
                            ingest_ts_ns_start: window_start_ingest.unwrap_or(ingest_ts),
                            ingest_ts_ns_end: ingest_ts,
                            exchange_ts_ns_start: window_start_exchange.unwrap_or(exchange_ts),
                            exchange_ts_ns_end: exchange_ts,
                            book_hash: [0u8; 16],
                            flags: 0,
                        };
                        let path = writer.write_snapshot(&metadata, &payload)?;
                        println!(
                            "snapshotter: wrote snapshot seq={} bids={} asks={} path={}",
                            message.msg.seq,
                            snapshot.bid_count,
                            snapshot.ask_count,
                            path.display()
                        );
                        planner.mark_snapshot(SystemTime::now());
                        window_start_ingest = None;
                        window_start_exchange = None;
                    }
                }
            }
        }

        engine.commit()?;
    }
}

struct CliConfig {
    archive_path: PathBuf,
    venue_id: u16,
    market_id: u32,
    start_mode: StartMode,
    min_interval: Option<Duration>,
    min_records: Option<u64>,
    min_bytes: Option<u64>,
}

fn parse_args(args: &[String]) -> Result<CliConfig, String> {
    let mut archive_path: Option<PathBuf> = None;
    let mut venue_id: Option<u16> = None;
    let mut market_id: Option<u32> = None;
    let mut start_mode = StartMode::ResumeStrict;
    let mut min_interval = Some(Duration::from_secs(10));
    let mut min_records = None;
    let mut min_bytes = None;

    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--archive" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --archive".to_string());
            }
            archive_path = Some(PathBuf::from(&args[i]));
        } else if let Some(value) = arg.strip_prefix("--archive=") {
            archive_path = Some(PathBuf::from(value));
        } else if arg == "--venue" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --venue".to_string());
            }
            venue_id = Some(parse_u16(&args[i], "--venue")?);
        } else if let Some(value) = arg.strip_prefix("--venue=") {
            venue_id = Some(parse_u16(value, "--venue")?);
        } else if arg == "--market" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --market".to_string());
            }
            market_id = Some(parse_u32(&args[i], "--market")?);
        } else if let Some(value) = arg.strip_prefix("--market=") {
            market_id = Some(parse_u32(value, "--market")?);
        } else if arg == "--start" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --start".to_string());
            }
            start_mode = parse_start_mode(&args[i])?;
        } else if let Some(value) = arg.strip_prefix("--start=") {
            start_mode = parse_start_mode(value)?;
        } else if arg == "--interval" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --interval".to_string());
            }
            min_interval = Some(parse_duration_secs(&args[i])?);
        } else if let Some(value) = arg.strip_prefix("--interval=") {
            min_interval = Some(parse_duration_secs(value)?);
        } else if arg == "--records" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --records".to_string());
            }
            min_records = Some(parse_u64(&args[i], "--records")?);
        } else if let Some(value) = arg.strip_prefix("--records=") {
            min_records = Some(parse_u64(value, "--records")?);
        } else if arg == "--bytes" {
            i += 1;
            if i >= args.len() {
                return Err("missing value for --bytes".to_string());
            }
            min_bytes = Some(parse_u64(&args[i], "--bytes")?);
        } else if let Some(value) = arg.strip_prefix("--bytes=") {
            min_bytes = Some(parse_u64(value, "--bytes")?);
        } else {
            return Err(format!("unknown argument: {arg}"));
        }
        i += 1;
    }

    let archive_path = archive_path.ok_or_else(|| "--archive is required".to_string())?;
    let venue_id = venue_id.ok_or_else(|| "--venue is required".to_string())?;
    let market_id = market_id.ok_or_else(|| "--market is required".to_string())?;

    Ok(CliConfig {
        archive_path,
        venue_id,
        market_id,
        start_mode,
        min_interval,
        min_records,
        min_bytes,
    })
}

fn parse_start_mode(value: &str) -> Result<StartMode, String> {
    match value {
        "resume" | "resume-strict" => Ok(StartMode::ResumeStrict),
        "resume-snapshot" => Ok(StartMode::ResumeSnapshot),
        "resume-latest" => Ok(StartMode::ResumeLatest),
        "latest" => Ok(StartMode::Latest),
        "earliest" => Ok(StartMode::Earliest),
        _ => Err(format!(
            "invalid --start value: {value}. Use resume-strict, resume-snapshot, resume-latest, latest, or earliest."
        )),
    }
}

fn parse_u16(value: &str, flag: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .map_err(|_| format!("invalid {flag} value: {value}"))
}

fn parse_u32(value: &str, flag: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("invalid {flag} value: {value}"))
}

fn parse_u64(value: &str, flag: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("invalid {flag} value: {value}"))
}

fn parse_duration_secs(value: &str) -> Result<Duration, String> {
    let secs = parse_u64(value, "--interval")?;
    Ok(Duration::from_secs(secs))
}

fn build_snapshot_payload(book: &chronicle::replay::L2Book) -> (L2Snapshot, Vec<u8>) {
    let (price_scale, size_scale) = book.scales();
    let bids: Vec<PriceLevelUpdate> = book
        .bids()
        .iter()
        .rev()
        .map(|(price, size)| PriceLevelUpdate {
            price: *price,
            size: *size,
        })
        .collect();
    let asks: Vec<PriceLevelUpdate> = book
        .asks()
        .iter()
        .map(|(price, size)| PriceLevelUpdate {
            price: *price,
            size: *size,
        })
        .collect();

    let snapshot = L2Snapshot {
        price_scale,
        size_scale,
        _pad0: 0,
        bid_count: bids.len() as u32,
        ask_count: asks.len() as u32,
    };

    let total_levels = bids.len() + asks.len();
    let mut payload =
        Vec::with_capacity(size_of::<L2Snapshot>() + total_levels * size_of::<PriceLevelUpdate>());

    let snapshot_bytes = unsafe {
        std::slice::from_raw_parts(
            &snapshot as *const L2Snapshot as *const u8,
            size_of::<L2Snapshot>(),
        )
    };
    payload.extend_from_slice(snapshot_bytes);

    let bids_bytes = unsafe {
        std::slice::from_raw_parts(
            bids.as_ptr() as *const u8,
            bids.len() * size_of::<PriceLevelUpdate>(),
        )
    };
    payload.extend_from_slice(bids_bytes);

    let asks_bytes = unsafe {
        std::slice::from_raw_parts(
            asks.as_ptr() as *const u8,
            asks.len() * size_of::<PriceLevelUpdate>(),
        )
    };
    payload.extend_from_slice(asks_bytes);

    (snapshot, payload)
}

fn print_usage() {
    eprintln!("Usage: snapshotter --archive <path> --venue <id> --market <id> [options]");
    eprintln!(
        "  --start <mode>        resume-strict|resume-snapshot|resume-latest|latest|earliest"
    );
    eprintln!("  --interval <secs>     minimum seconds between snapshots (default: 10)");
    eprintln!("  --records <count>     minimum records between snapshots");
    eprintln!("  --bytes <count>       minimum bytes between snapshots");
}
