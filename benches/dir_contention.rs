use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::tempdir;

use chronicle::core::header::{HEADER_SIZE, RECORD_ALIGN};
use chronicle::core::segment::SEG_DATA_OFFSET;
use chronicle::core::{Queue, WriterConfig};

fn main() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("bench_queue");

    let segment_size = 4096_u64;
    let payload_len = (segment_size as usize)
        .saturating_sub(SEG_DATA_OFFSET + HEADER_SIZE)
        / RECORD_ALIGN
        * RECORD_ALIGN;
    let config = WriterConfig {
        segment_size_bytes: segment_size,
        ..WriterConfig::default()
    };
    let mut writer = Queue::open_publisher_with_config(&path, config).expect("writer");
    let payload = vec![0u8; payload_len];

    // Warm-up to avoid setup skew.
    writer.append(1, &payload).expect("append warm-up");

    let running = Arc::new(AtomicBool::new(true));
    let bg_running = running.clone();
    let bg_path = path.clone();
    let cleanup_thread = thread::spawn(move || {
        while bg_running.load(Ordering::Relaxed) {
            let mut segments = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&bg_path) {
                for entry in entries.flatten() {
                    let entry_path = entry.path();
                    let Some(name) = entry_path.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    let Some(stripped) = name.strip_suffix(".q") else {
                        continue;
                    };
                    if let Ok(id) = stripped.parse::<u64>() {
                        segments.push((id, entry_path));
                    }
                }
            }
            segments.sort_by_key(|(id, _)| *id);
            if segments.len() > 2 {
                for (_, old_path) in segments.iter().take(segments.len() - 2) {
                    let _ = std::fs::remove_file(old_path);
                }
            }
            thread::sleep(Duration::from_millis(1));
        }
    });

    let iterations = 10_000_usize;
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        writer.append(1, &payload).expect("append");
        samples.push(start.elapsed());
    }

    running.store(false, Ordering::Relaxed);
    let _ = cleanup_thread.join();

    samples.sort();
    let p50 = samples[iterations / 2];
    let p99 = samples[(iterations * 99) / 100];
    println!("roll p50: {:?}, p99: {:?}", p50, p99);
}
