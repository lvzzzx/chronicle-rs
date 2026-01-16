use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use chronicle_core::{Error, MessageView, Queue, Result};
use tempfile::tempdir;

const PAYLOAD_LEN: usize = 16;
const PAYLOAD_TAG: [u8; 8] = *b"chrcore!";
const TYPE_ID: u16 = 1;
const DEFAULT_SOAK_SECS: u64 = 30;

#[test]
#[ignore]
fn soak_writer_reader() -> Result<()> {
    let secs = env_u64("CHRONICLE_SOAK_SECS", DEFAULT_SOAK_SECS).max(1);
    run_soak(Duration::from_secs(secs))
}

fn run_soak(duration: Duration) -> Result<()> {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("soak_queue");

    let mut writer = Queue::open_publisher(&queue_path)?;
    let mut reader = Queue::open_subscriber(&queue_path, "soak")?;

    let running = Arc::new(AtomicBool::new(true));
    let written = Arc::new(AtomicU64::new(0));

    let running_writer = Arc::clone(&running);
    let written_writer = Arc::clone(&written);

    let handle = std::thread::spawn(move || -> Result<()> {
        let mut seq: u64 = 0;
        while running_writer.load(Ordering::Relaxed) {
            let payload = build_payload(seq);
            writer.append(TYPE_ID, &payload)?;
            written_writer.fetch_add(1, Ordering::Relaxed);
            seq = seq.wrapping_add(1);

            if seq % 4096 == 0 {
                writer.cleanup()?;
                writer.flush_async()?;
            }
            if seq % 256 == 0 {
                std::thread::sleep(Duration::from_micros(50));
            }
        }
        writer.flush_sync()?;
        Ok(())
    });

    let start = Instant::now();
    let mut expected_seq: u64 = 0;
    let mut read: u64 = 0;
    while start.elapsed() < duration {
        if let Some(message) = reader.next()? {
            validate_message(&message, expected_seq)?;
            expected_seq += 1;
            read += 1;
            if read % 4096 == 0 {
                reader.commit()?;
            }
        } else {
            reader.wait(Some(Duration::from_millis(5)))?;
        }
    }

    running.store(false, Ordering::Relaxed);
    handle.join().expect("writer thread")?;

    let target = written.load(Ordering::Relaxed);
    while read < target {
        if let Some(message) = reader.next()? {
            validate_message(&message, expected_seq)?;
            expected_seq += 1;
            read += 1;
        } else {
            reader.wait(Some(Duration::from_millis(5)))?;
        }
    }

    reader.commit()?;
    if read != target {
        return Err(Error::Corrupt("soak read/write mismatch"));
    }

    Ok(())
}

fn build_payload(seq: u64) -> [u8; PAYLOAD_LEN] {
    let mut payload = [0u8; PAYLOAD_LEN];
    payload[0..8].copy_from_slice(&seq.to_le_bytes());
    payload[8..16].copy_from_slice(&PAYLOAD_TAG);
    payload
}

fn validate_message(message: &MessageView<'_>, expected_seq: u64) -> Result<()> {
    if message.seq != expected_seq {
        return Err(Error::Corrupt("sequence mismatch"));
    }
    if message.type_id != TYPE_ID {
        return Err(Error::Corrupt("type id mismatch"));
    }
    if message.payload.len() != PAYLOAD_LEN {
        return Err(Error::Corrupt("payload length mismatch"));
    }
    let seq = u64::from_le_bytes(
        message.payload[0..8]
            .try_into()
            .map_err(|_| Error::Corrupt("payload decode"))?,
    );
    if seq != expected_seq {
        return Err(Error::Corrupt("payload seq mismatch"));
    }
    if message.payload[8..16] != PAYLOAD_TAG {
        return Err(Error::Corrupt("payload tag mismatch"));
    }
    Ok(())
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}
