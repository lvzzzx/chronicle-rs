use std::collections::VecDeque;
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use chronicle::core::control::ControlFile;
use chronicle::core::retention::retention_candidates;
use chronicle::core::segment::{load_index, load_reader_meta, validate_segment_size};
use chronicle::core::{lock_owner_alive, read_lock_info, Queue, WriterLockInfo};

mod monitor;

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
    writeln!(out, "segment_size={} writer_epoch={}", segment_size, control.writer_epoch())?;
    writeln!(
        out,
        "writer_heartbeat_ns={}",
        control.writer_heartbeat_ns()
    )?;

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

        let candidates = retention_candidates(
            &queue,
            head_segment as u64,
            head_offset,
            segment_size,
        )?;
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

    let unique = format!(
        "chronicle_bench_{}_{}",
        std::process::id(),
        now_ns()?
    );
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
