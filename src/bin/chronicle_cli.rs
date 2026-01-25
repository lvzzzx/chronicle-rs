use std::collections::VecDeque;
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chronicle::cli::monitor;
use chronicle::core::control::ControlFile;
use chronicle::core::retention::retention_candidates;
use chronicle::core::segment::{load_index, load_reader_meta, validate_segment_size};
use chronicle::core::{lock_owner_alive, read_lock_info, Queue, WriterLockInfo};
use chronicle::protocol::{BookEventType, BookMode, L2Diff, L2Snapshot, TypeId};
use chronicle::replay::{L2Book, LevelsView};
use chronicle::storage::StorageReader;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "chron-cli", version, about = "Chronicle queue tooling")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Inspect {
        queue_path: PathBuf,
    },
    Tail {
        queue_path: PathBuf,
        #[arg(short = 'f', long = "follow")]
        follow: bool,
        #[arg(long = "reader")]
        reader: Option<String>,
        #[arg(long = "limit")]
        limit: Option<usize>,
    },
    Doctor {
        bus_root: PathBuf,
    },
    Bench {
        #[arg(long = "queue-path")]
        queue_path: Option<PathBuf>,
        #[arg(long = "messages", default_value_t = 100_000)]
        messages: u64,
        #[arg(long = "payload-bytes", default_value_t = 256)]
        payload_bytes: usize,
        #[arg(long = "read")]
        read: bool,
        #[arg(long = "keep")]
        keep: bool,
    },
    ArchiveTail {
        stream_dir: PathBuf,
        #[arg(long = "limit")]
        limit: Option<usize>,
        #[arg(long = "hexdump")]
        hexdump: bool,
        #[arg(long = "decode-book")]
        decode_book: bool,
    },
    ArchiveReplay {
        stream_dir: PathBuf,
        #[arg(long = "limit")]
        limit: Option<usize>,
    },
    Monitor(monitor::MonitorArgs),
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let mut out = io::BufWriter::new(io::stdout());
    match cli.command {
        Commands::Inspect { queue_path } => cmd_inspect(&queue_path, &mut out)?,
        Commands::Tail {
            queue_path,
            follow,
            reader,
            limit,
        } => cmd_tail(&queue_path, follow, reader, limit, &mut out)?,
        Commands::Doctor { bus_root } => cmd_doctor(&bus_root, &mut out)?,
        Commands::Bench {
            queue_path,
            messages,
            payload_bytes,
            read,
            keep,
        } => cmd_bench(&queue_path, messages, payload_bytes, read, keep, &mut out)?,
        Commands::ArchiveTail {
            stream_dir,
            limit,
            hexdump,
            decode_book,
        } => cmd_archive_tail(&stream_dir, limit, hexdump, decode_book, &mut out)?,
        Commands::ArchiveReplay { stream_dir, limit } => {
            cmd_archive_replay(&stream_dir, limit, &mut out)?;
        }
        Commands::Monitor(args) => {
            monitor::run(args)?;
        }
    }
    Ok(())
}

fn cmd_inspect(queue_path: &Path, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let control_path = queue_path.join("control.meta");
    let control = ControlFile::open(&control_path)?;
    control.wait_ready()?;
    let (head_segment, head_offset) = control.segment_index();
    let segment_size = validate_segment_size(control.segment_size())? as u64;
    let head_pos = (head_segment as u64)
        .saturating_mul(segment_size)
        .saturating_add(head_offset);
    writeln!(out, "queue={}", queue_path.display())?;
    writeln!(
        out,
        "head_segment={} head_offset={} head_pos={}",
        head_segment, head_offset, head_pos
    )?;
    writeln!(
        out,
        "segment_size={} writer_epoch={}",
        segment_size,
        control.writer_epoch()
    )?;
    writeln!(out, "writer_heartbeat_ns={}", control.writer_heartbeat_ns())?;

    let index_path = queue_path.join("index.meta");
    if index_path.exists() {
        let index = load_index(&index_path)?;
        writeln!(
            out,
            "index_segment={} index_offset={}",
            index.current_segment, index.write_offset
        )?;
    }

    let lock_path = queue_path.join("writer.lock");
    write_lock_status(read_lock_info(&lock_path)?, out)?;

    let readers_dir = queue_path.join("readers");
    if readers_dir.exists() {
        let mut entries: Vec<_> = fs::read_dir(&readers_dir)?
            .filter_map(|entry| entry.ok())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("meta") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("unknown");
            let meta = load_reader_meta(&path)?;
            let reader_pos = meta
                .segment_id
                .saturating_mul(segment_size)
                .saturating_add(meta.offset);
            let lag_bytes = head_pos.saturating_sub(reader_pos);
            let lag_segments = (head_segment as u64).saturating_sub(meta.segment_id);
            writeln!(
                out,
                "reader name={} segment={} offset={} lag_bytes={} lag_segments={} heartbeat_ns={}",
                name, meta.segment_id, meta.offset, lag_bytes, lag_segments, meta.last_heartbeat_ns
            )?;
        }
    }

    Ok(())
}

fn cmd_tail(
    queue_path: &Path,
    follow: bool,
    reader: Option<String>,
    limit: Option<usize>,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let auto_reader = reader.is_none();
    let reader_name = reader.unwrap_or_else(|| format!("cli_tail_{}", std::process::id()));
    let result = run_tail_loop(queue_path, &reader_name, follow, limit, out);

    if auto_reader {
        let meta_path = queue_path
            .join("readers")
            .join(format!("{reader_name}.meta"));
        let _ = fs::remove_file(meta_path);
    }

    result
}

fn run_tail_loop(
    queue_path: &Path,
    reader_name: &str,
    follow: bool,
    limit: Option<usize>,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let mut reader = Queue::open_subscriber(queue_path, reader_name)?;
    let mut seen = 0usize;
    loop {
        if let Some(message) = reader.next()? {
            writeln!(
                out,
                "seq={} ts={} type={} len={}",
                message.seq,
                message.timestamp_ns,
                message.type_id,
                message.payload.len()
            )?;
            print_hexdump(message.payload, out)?;
            reader.commit()?;
            seen = seen.saturating_add(1);
            if limit.map_or(false, |limit| seen >= limit) {
                break;
            }
            continue;
        }

        if follow {
            reader.wait(None)?;
            continue;
        }
        break;
    }
    Ok(())
}

fn cmd_doctor(bus_root: &Path, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let queues = find_queue_roots(bus_root)?;
    if queues.is_empty() {
        writeln!(out, "no queues found under {}", bus_root.display())?;
        return Ok(());
    }

    for queue in queues {
        writeln!(out, "queue={}", queue.display())?;
        let control_path = queue.join("control.meta");
        let control = match ControlFile::open(&control_path) {
            Ok(control) => control,
            Err(err) => {
                writeln!(out, "control_error={err}")?;
                continue;
            }
        };
        if let Err(err) = control.wait_ready() {
            writeln!(out, "control_error={err}")?;
            continue;
        }
        let (head_segment, head_offset) = control.segment_index();
        let segment_size = validate_segment_size(control.segment_size())? as u64;
        let lock_path = queue.join("writer.lock");
        write_lock_status(read_lock_info(&lock_path)?, out)?;

        let candidates =
            retention_candidates(&queue, head_segment as u64, head_offset, segment_size)?;
        if candidates.is_empty() {
            writeln!(out, "retention_candidates count=0")?;
        } else {
            let first = candidates.first().copied().unwrap_or(0);
            let last = candidates.last().copied().unwrap_or(0);
            writeln!(
                out,
                "retention_candidates count={} range={}..={}",
                candidates.len(),
                first,
                last
            )?;
        }
    }
    Ok(())
}

fn cmd_bench(
    queue_path: &Option<PathBuf>,
    messages: u64,
    payload_bytes: usize,
    read: bool,
    keep: bool,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let (queue_path, cleanup) = prepare_bench_path(queue_path, keep)?;
    writeln!(out, "queue_path={}", queue_path.display())?;

    let payload = vec![0u8; payload_bytes];
    let mut writer = Queue::open_publisher(&queue_path)?;
    let timestamp_ns = now_ns()?;
    let start = Instant::now();
    for _ in 0..messages {
        writer.append_with_timestamp(1, &payload, timestamp_ns)?;
    }
    writer.flush_sync()?;
    let elapsed = start.elapsed();
    report_throughput(out, "write", messages, payload_bytes, elapsed)?;

    if read {
        let reader_name = format!("bench_reader_{}", std::process::id());
        let mut reader = Queue::open_subscriber(&queue_path, &reader_name)?;
        let mut seen = 0u64;
        let start = Instant::now();
        while seen < messages {
            if let Some(_) = reader.next()? {
                reader.commit()?;
                seen = seen.saturating_add(1);
            } else {
                reader.wait(Some(Duration::from_millis(1)))?;
            }
        }
        let elapsed = start.elapsed();
        report_throughput(out, "read", messages, payload_bytes, elapsed)?;
    }

    if cleanup && !keep {
        fs::remove_dir_all(&queue_path)?;
        writeln!(out, "cleanup=removed")?;
    } else if keep {
        writeln!(out, "cleanup=kept")?;
    }

    Ok(())
}

fn cmd_archive_tail(
    stream_dir: &Path,
    limit: Option<usize>,
    hexdump: bool,
    decode_book: bool,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let segments = resolve_archive_segments(stream_dir)?;
    if segments.is_empty() {
        writeln!(out, "no segments found under {}", stream_dir.display())?;
        return Ok(());
    }

    let mut seen = 0usize;
    for segment in segments {
        writeln!(out, "segment={}", segment.display())?;
        let mut reader = StorageReader::open_segment(&segment)?;
        while let Some(message) = reader.next()? {
            writeln!(
                out,
                "seq={} ts={} type={} len={}",
                message.seq,
                message.timestamp_ns,
                message.type_id,
                message.payload.len()
            )?;
            if hexdump {
                print_hexdump(message.payload, out)?;
            }
            if decode_book && message.type_id == TypeId::BookEvent.as_u16() {
                if let Some(header) = decode_book_header(message.payload) {
                    writeln!(
                        out,
                        "book schema={} record_len={} event_type={} book_mode={} venue={} market={} exchange_ts_ns={} ingest_ts_ns={} native_seq={}",
                        header.schema_version,
                        header.record_len,
                        header.event_type,
                        header.book_mode,
                        header.venue_id,
                        header.market_id,
                        header.exchange_ts_ns,
                        header.ingest_ts_ns,
                        header.native_seq
                    )?;
                }
            }
            seen = seen.saturating_add(1);
            if limit.map_or(false, |limit| seen >= limit) {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn cmd_archive_replay(
    stream_dir: &Path,
    limit: Option<usize>,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let segments = resolve_archive_segments(stream_dir)?;
    if segments.is_empty() {
        writeln!(out, "no segments found under {}", stream_dir.display())?;
        return Ok(());
    }

    let mut book = L2Book::new();
    let mut applied = 0usize;
    let mut skipped = 0usize;
    let mut last_header: Option<BookHeaderView> = None;
    let mut last_seq = 0u64;

    for segment in segments {
        let mut reader = StorageReader::open_segment(&segment)?;
        while let Some(message) = reader.next()? {
            last_seq = message.seq;
            if message.type_id != TypeId::BookEvent.as_u16() {
                skipped = skipped.saturating_add(1);
                continue;
            }
            let Some(header) = decode_book_header(message.payload) else {
                skipped = skipped.saturating_add(1);
                continue;
            };
            last_header = Some(header);
            if header.book_mode != BookMode::L2 as u8 {
                skipped = skipped.saturating_add(1);
                continue;
            }
            let header_size = if header.schema_version >= 2 { 64 } else { 56 };
            let payload_offset = header_size;

            match header.event_type {
                x if x == BookEventType::Snapshot as u8 => {
                    let Some(snapshot) = read_copy::<L2Snapshot>(message.payload, payload_offset)
                    else {
                        skipped = skipped.saturating_add(1);
                        continue;
                    };
                    let levels_offset = payload_offset + std::mem::size_of::<L2Snapshot>();
                    let Some(levels) = LevelsView::new(
                        message.payload,
                        levels_offset,
                        snapshot.bid_count as usize,
                        snapshot.ask_count as usize,
                    ) else {
                        skipped = skipped.saturating_add(1);
                        continue;
                    };
                    book.apply_snapshot(&snapshot, levels.bids_iter(), levels.asks_iter());
                    applied = applied.saturating_add(1);
                }
                x if x == BookEventType::Diff as u8 => {
                    let Some(diff) = read_copy::<L2Diff>(message.payload, payload_offset) else {
                        skipped = skipped.saturating_add(1);
                        continue;
                    };
                    let levels_offset = payload_offset + std::mem::size_of::<L2Diff>();
                    let Some(levels) = LevelsView::new(
                        message.payload,
                        levels_offset,
                        diff.bid_count as usize,
                        diff.ask_count as usize,
                    ) else {
                        skipped = skipped.saturating_add(1);
                        continue;
                    };
                    book.apply_diff(&diff, levels.bids_iter(), levels.asks_iter());
                    applied = applied.saturating_add(1);
                }
                _ => {
                    skipped = skipped.saturating_add(1);
                }
            }

            if limit.map_or(false, |limit| applied >= limit) {
                break;
            }
        }

        if limit.map_or(false, |limit| applied >= limit) {
            break;
        }
    }

    let best_bid = book.bids().iter().next_back().map(|(p, s)| (*p, *s));
    let best_ask = book.asks().iter().next().map(|(p, s)| (*p, *s));
    let (price_scale, size_scale) = book.scales();

    writeln!(
        out,
        "replay applied={} skipped={} last_seq={} price_scale={} size_scale={}",
        applied, skipped, last_seq, price_scale, size_scale
    )?;
    writeln!(out, "bids={} asks={}", book.bids().len(), book.asks().len())?;
    if let Some((price, size)) = best_bid {
        writeln!(out, "best_bid price={} size={}", price, size)?;
    }
    if let Some((price, size)) = best_ask {
        writeln!(out, "best_ask price={} size={}", price, size)?;
    }
    if let Some(header) = last_header {
        writeln!(
            out,
            "last_exchange_ts_ns={} last_ingest_ts_ns={}",
            header.exchange_ts_ns, header.ingest_ts_ns
        )?;
    }

    Ok(())
}

fn resolve_archive_segments(stream_dir: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    if stream_dir.is_file() {
        return Ok(vec![stream_dir.to_path_buf()]);
    }
    if !stream_dir.is_dir() {
        return Err(format!("stream dir not found: {}", stream_dir.display()).into());
    }

    #[derive(Default)]
    struct SegmentPaths {
        q: Option<PathBuf>,
        zst: Option<PathBuf>,
    }

    let mut segments = std::collections::BTreeMap::<u64, SegmentPaths>::new();
    for entry in fs::read_dir(stream_dir)? {
        let dir_entry = entry?;
        if !dir_entry.file_type()?.is_file() {
            continue;
        }
        let file_name = dir_entry.file_name();
        let name = file_name.to_string_lossy();
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

fn read_copy<T: Copy>(buf: &[u8], offset: usize) -> Option<T> {
    let size = std::mem::size_of::<T>();
    if buf.len() < offset + size {
        return None;
    }
    let ptr = unsafe { buf.as_ptr().add(offset) as *const T };
    Some(unsafe { std::ptr::read_unaligned(ptr) })
}

#[allow(dead_code)]
#[derive(Clone, Copy)]
struct BookHeaderView {
    schema_version: u16,
    record_len: u32,
    endianness: u8,
    venue_id: u16,
    market_id: u32,
    stream_id: u32,
    ingest_ts_ns: u64,
    exchange_ts_ns: u64,
    seq: u64,
    native_seq: u64,
    event_type: u8,
    book_mode: u8,
    flags: u16,
}

fn decode_book_header(buf: &[u8]) -> Option<BookHeaderView> {
    if buf.len() < 56 {
        return None;
    }
    let schema_version = u16::from_le_bytes(buf[0..2].try_into().ok()?);
    if schema_version >= 2 {
        if buf.len() < 64 {
            return None;
        }
        Some(BookHeaderView {
            schema_version,
            record_len: u32::from_le_bytes(buf[4..8].try_into().ok()?),
            endianness: *buf.get(8)?,
            venue_id: u16::from_le_bytes(buf[10..12].try_into().ok()?),
            market_id: u32::from_le_bytes(buf[12..16].try_into().ok()?),
            stream_id: u32::from_le_bytes(buf[16..20].try_into().ok()?),
            ingest_ts_ns: u64::from_le_bytes(buf[24..32].try_into().ok()?),
            exchange_ts_ns: u64::from_le_bytes(buf[32..40].try_into().ok()?),
            seq: u64::from_le_bytes(buf[40..48].try_into().ok()?),
            native_seq: u64::from_le_bytes(buf[48..56].try_into().ok()?),
            event_type: *buf.get(56)?,
            book_mode: *buf.get(57)?,
            flags: u16::from_le_bytes(buf[58..60].try_into().ok()?),
        })
    } else {
        Some(BookHeaderView {
            schema_version,
            record_len: u16::from_le_bytes(buf[2..4].try_into().ok()?) as u32,
            endianness: *buf.get(4)?,
            venue_id: u16::from_le_bytes(buf[6..8].try_into().ok()?),
            market_id: u32::from_le_bytes(buf[8..12].try_into().ok()?),
            stream_id: u32::from_le_bytes(buf[12..16].try_into().ok()?),
            ingest_ts_ns: u64::from_le_bytes(buf[16..24].try_into().ok()?),
            exchange_ts_ns: u64::from_le_bytes(buf[24..32].try_into().ok()?),
            seq: u64::from_le_bytes(buf[32..40].try_into().ok()?),
            native_seq: u64::from_le_bytes(buf[40..48].try_into().ok()?),
            event_type: *buf.get(48)?,
            book_mode: *buf.get(49)?,
            flags: u16::from_le_bytes(buf[50..52].try_into().ok()?),
        })
    }
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

fn prepare_bench_path(
    queue_path: &Option<PathBuf>,
    keep: bool,
) -> Result<(PathBuf, bool), Box<dyn Error>> {
    if let Some(path) = queue_path {
        let created = !path.exists();
        if !created {
            let mut entries = fs::read_dir(path)?;
            if entries.next().is_some() {
                return Err(format!(
                    "queue path {} is not empty; remove it or choose another path",
                    path.display()
                )
                .into());
            }
        } else {
            fs::create_dir_all(path)?;
        }
        let cleanup = !keep && created;
        return Ok((path.clone(), cleanup));
    }

    let unique = format!("chronicle_bench_{}_{}", std::process::id(), now_ns()?);
    let path = std::env::temp_dir().join(unique);
    fs::create_dir_all(&path)?;
    Ok((path, !keep))
}

fn find_queue_roots(root: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut found = Vec::new();
    let mut pending = VecDeque::new();
    pending.push_back(root.to_path_buf());

    while let Some(dir) = pending.pop_front() {
        let control_path = dir.join("control.meta");
        if control_path.exists() {
            found.push(dir);
            continue;
        }

        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) => {
                if err.kind() == io::ErrorKind::NotFound {
                    continue;
                }
                return Err(err.into());
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                pending.push_back(entry.path());
            }
        }
    }

    found.sort();
    Ok(found)
}

fn write_lock_status(
    info: Option<WriterLockInfo>,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    match info {
        Some(info) if info.pid == 0 && info.start_time == 0 && info.epoch == 0 => {
            writeln!(out, "lock none")?;
        }
        Some(info) => {
            let alive = lock_owner_alive(&info)?;
            writeln!(
                out,
                "lock pid={} start_time={} epoch={} alive={}",
                info.pid, info.start_time, info.epoch, alive
            )?;
        }
        None => {
            writeln!(out, "lock none")?;
        }
    }
    Ok(())
}

fn report_throughput(
    out: &mut dyn Write,
    label: &str,
    messages: u64,
    payload_bytes: usize,
    elapsed: Duration,
) -> Result<(), Box<dyn Error>> {
    let secs = elapsed.as_secs_f64().max(1e-9);
    let msg_per_sec = messages as f64 / secs;
    let total_bytes = (payload_bytes as f64) * (messages as f64);
    let mb_per_sec = (total_bytes / (1024.0 * 1024.0)) / secs;
    writeln!(
        out,
        "{label}_elapsed_ms={} {label}_msg_per_sec={:.2} {label}_mb_per_sec={:.2}",
        elapsed.as_millis(),
        msg_per_sec,
        mb_per_sec
    )?;
    Ok(())
}

fn print_hexdump(payload: &[u8], out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    for (index, chunk) in payload.chunks(16).enumerate() {
        write!(out, "{:04x}:", index * 16)?;
        for byte in chunk {
            write!(out, " {:02x}", byte)?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn now_ns() -> Result<u64, Box<dyn Error>> {
    let duration = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let nanos = u64::try_from(duration.as_nanos())?;
    Ok(nanos)
}
