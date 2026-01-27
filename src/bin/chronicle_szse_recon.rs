use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, Result};
use clap::Parser;

use chronicle::venues::szse::l3::{
    decode_l3_message, write_checkpoint_json, ChannelCheckpoint, ChannelSequencer, DecodePolicy,
    GapPolicy, L3Message, SymbolCheckpoint, SzseL3Worker, UnknownOrderPolicy,
};
use chronicle::protocol::{BookEventHeader, BookEventType, BookMode};
use chronicle::storage::StorageReader;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{self, SyncSender};
use std::thread;

#[derive(Parser, Debug)]
#[command(about = "Reconstruct SZSE L3 order books from a per-channel archive")]
struct Args {
    /// Stream directory containing segment files (e.g. .../v1/szse/2024-09-30/2011)
    #[arg(long)]
    stream_dir: Option<PathBuf>,

    /// Archive root (used with --date and --channel to locate a stream dir)
    #[arg(long)]
    archive_root: Option<PathBuf>,

    /// Trading date in YYYY-MM-DD (used with --archive-root)
    #[arg(long)]
    date: Option<String>,

    /// Channel id (used with --archive-root)
    #[arg(long)]
    channel: Option<u32>,

    /// Number of worker shards for per-symbol book engines
    #[arg(long)]
    workers: Option<usize>,

    /// Bounded queue capacity per worker
    #[arg(long, default_value = "65536")]
    queue_capacity: usize,

    /// Stop after N messages (for quick checks)
    #[arg(long)]
    limit: Option<u64>,

    /// Filter to a specific symbol (e.g. 000001)
    #[arg(long)]
    symbol: Option<String>,

    /// Write a JSON checkpoint to this path on completion
    #[arg(long)]
    checkpoint: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let stream_dir = resolve_stream_dir(&args)?;
    let segments = resolve_archive_segments(&stream_dir)?;
    if segments.is_empty() {
        return Err(anyhow!("no segments found under {}", stream_dir.display()));
    }

    let worker_count = args
        .workers
        .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()))
        .unwrap_or(1);
    let mut sequencer = ChannelSequencer::new(GapPolicy::Fail);
    let decode_policy = DecodePolicy::Fail;
    let unknown_order = UnknownOrderPolicy::Fail;
    let queue_capacity = args.queue_capacity.max(1);

    let mut senders: Vec<SyncSender<L3Message>> = Vec::with_capacity(worker_count);
    let mut handles = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        let (tx, rx) = mpsc::sync_channel(queue_capacity);
        senders.push(tx);
        let handle = thread::spawn(move || -> Result<Vec<SymbolCheckpoint>> {
            let mut worker = SzseL3Worker::default();
            while let Ok(msg) = rx.recv() {
                worker.apply_message(&msg.header, &msg.event, unknown_order)?;
            }
            Ok(worker.checkpoints())
        });
        handles.push(handle);
    }

    let mut applied = 0u64;
    let mut skipped = 0u64;
    let mut filtered = 0u64;
    let mut total = 0u64;
    let start = Instant::now();
    let mut channel_id: Option<u32> = None;
    let symbol_filter = args
        .symbol
        .as_deref()
        .map(parse_symbol)
        .transpose()?;

    'segments: for segment in segments {
        let mut reader = StorageReader::open_segment(&segment)?;
        while let Some(message) = reader.next()? {
            total = total.saturating_add(1);
            let Some(header) = read_book_header(message.payload) else {
                skipped = skipped.saturating_add(1);
                continue;
            };

            if header.book_mode != BookMode::L3 as u8 || header.event_type != BookEventType::Diff as u8 {
                skipped = skipped.saturating_add(1);
                continue;
            }

            if let Some(expected) = args.channel {
                if header.stream_id != expected {
                    return Err(anyhow!(
                        "unexpected channel {}, expected {}",
                        header.stream_id,
                        expected
                    ));
                }
            }
            if let Some(expected) = channel_id {
                if header.stream_id != expected {
                    return Err(anyhow!(
                        "unexpected channel {}, expected {}",
                        header.stream_id,
                        expected
                    ));
                }
            } else {
                channel_id = Some(header.stream_id);
            }

            sequencer.check(header.seq)?;
            if let Some(target) = symbol_filter {
                if header.market_id != target {
                    filtered = filtered.saturating_add(1);
                    continue;
                }
            }

            let Some(l3) = decode_l3_message(&message, decode_policy)? else {
                skipped = skipped.saturating_add(1);
                continue;
            };
            let worker_idx = worker_index(l3.header.market_id, worker_count);
            senders[worker_idx].send(l3)?;
            applied = applied.saturating_add(1);
            if args.limit.map_or(false, |limit| total >= limit) {
                break 'segments;
            }
        }
    }

    drop(senders);

    let mut symbols = Vec::new();
    for handle in handles {
        let worker_symbols = handle
            .join()
            .map_err(|_| anyhow!("worker thread panicked"))??;
        symbols.extend(worker_symbols);
    }
    symbols.sort_by_key(|entry| entry.symbol);
    symbols.dedup_by_key(|entry| entry.symbol);

    let elapsed = start.elapsed();
    let msg_per_sec = if elapsed.as_secs_f64() > 0.0 {
        total as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };

    let checkpoint = channel_id
        .and_then(|channel| sequencer.last_seq().map(|last_seq| ChannelCheckpoint {
            channel,
            last_seq,
            symbols,
        }));
    if let Some(checkpoint) = checkpoint.as_ref() {
        if let Some(path) = args.checkpoint.as_ref() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            write_checkpoint_json(path, checkpoint)?;
        }
        println!(
            "channel={} last_seq={} symbols={} total={} applied={} skipped={} filtered={} rate={:.2} msg/s",
            checkpoint.channel,
            checkpoint.last_seq,
            checkpoint.symbols.len(),
            total,
            applied,
            skipped,
            filtered,
            msg_per_sec
        );
    } else {
        println!(
            "total={} applied={} skipped={} filtered={} rate={:.2} msg/s",
            total, applied, skipped, filtered, msg_per_sec
        );
    }

    Ok(())
}

fn resolve_stream_dir(args: &Args) -> Result<PathBuf> {
    if let Some(dir) = args.stream_dir.as_ref() {
        return Ok(dir.clone());
    }
    let root = args
        .archive_root
        .as_ref()
        .ok_or_else(|| anyhow!("--stream-dir or --archive-root required"))?;
    let date = args
        .date
        .as_ref()
        .ok_or_else(|| anyhow!("--date required when using --archive-root"))?;
    let channel = args
        .channel
        .ok_or_else(|| anyhow!("--channel required when using --archive-root"))?;
    Ok(root.join("v1").join("szse").join(date).join(channel.to_string()))
}

fn resolve_archive_segments(stream_dir: &Path) -> Result<Vec<PathBuf>> {
    if stream_dir.is_file() {
        return Ok(vec![stream_dir.to_path_buf()]);
    }
    if !stream_dir.is_dir() {
        return Err(anyhow!("stream dir not found: {}", stream_dir.display()));
    }

    #[derive(Default)]
    struct SegmentPaths {
        q: Option<PathBuf>,
        zst: Option<PathBuf>,
    }

    let mut segments = std::collections::BTreeMap::<u64, SegmentPaths>::new();
    for dir_entry in fs::read_dir(stream_dir)? {
        let dir_entry = dir_entry?;
        if !dir_entry.file_type()?.is_file() {
            continue;
        }
        let name = dir_entry.file_name();
        let name = name.to_string_lossy();
        let (id, is_zst) = match parse_segment_name(&name) {
            Some(parsed) => parsed,
            None => continue,
        };
        let entry = segments.entry(id).or_default();
        let path = dir_entry.path();
        if is_zst {
            entry.zst = Some(path);
        } else {
            entry.q = Some(path);
        }
    }

    let mut out = Vec::new();
    for (_id, paths) in segments {
        if let Some(zst) = paths.zst {
            out.push(zst);
        } else if let Some(q) = paths.q {
            out.push(q);
        }
    }
    Ok(out)
}

fn parse_segment_name(name: &str) -> Option<(u64, bool)> {
    if let Some(stem) = name.strip_suffix(".q.zst") {
        if stem.chars().all(|c| c.is_ascii_digit()) {
            return stem.parse::<u64>().ok().map(|id| (id, true));
        }
    }
    if let Some(stem) = name.strip_suffix(".q") {
        if stem.chars().all(|c| c.is_ascii_digit()) {
            return stem.parse::<u64>().ok().map(|id| (id, false));
        }
    }
    None
}

fn worker_index(symbol: u32, worker_count: usize) -> usize {
    let mut hasher = DefaultHasher::new();
    symbol.hash(&mut hasher);
    (hasher.finish() as usize) % worker_count
}

fn parse_symbol(raw: &str) -> Result<u32> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("symbol is empty"));
    }
    if !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Err(anyhow!("symbol must be numeric, got {}", trimmed));
    }
    trimmed
        .parse::<u32>()
        .map_err(|_| anyhow!("symbol out of range: {}", trimmed))
}

fn read_book_header(buf: &[u8]) -> Option<BookEventHeader> {
    let size = std::mem::size_of::<BookEventHeader>();
    if buf.len() < size {
        return None;
    }
    let ptr = unsafe { buf.as_ptr() as *const BookEventHeader };
    Some(unsafe { std::ptr::read_unaligned(ptr) })
}
