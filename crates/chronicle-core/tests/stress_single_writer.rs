use std::sync::mpsc;
use std::time::{Duration, Instant};

use chronicle_core::{Error, MessageView, Queue, Result, WriterConfig};
use tempfile::tempdir;

const PAYLOAD_LEN: usize = 16;
const PAYLOAD_TAG: [u8; 8] = *b"chrcore!";
const TYPE_ID: u16 = 1;
const FAST_MESSAGES: usize = 20_000;
const DEFAULT_STRESS_MESSAGES: usize = 200_000;
const TEST_SEGMENT_SIZE: usize = 1 * 1024 * 1024;

#[test]
fn stress_single_writer_fast() -> Result<()> {
    run_stress(FAST_MESSAGES, Duration::from_secs(15))
}

#[test]
#[ignore]
fn stress_single_writer_heavy() -> Result<()> {
    let messages = env_usize("CHRONICLE_STRESS_MESSAGES", DEFAULT_STRESS_MESSAGES);
    run_stress(messages, Duration::from_secs(120))
}

fn run_stress(messages: usize, max_idle: Duration) -> Result<()> {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("stress_queue");

    let mut writer = Queue::open_publisher_with_config(
        &queue_path,
        WriterConfig {
            segment_size_bytes: TEST_SEGMENT_SIZE as u64,
            ..WriterConfig::default()
        },
    )?;
    let mut reader = Queue::open_subscriber(&queue_path, "stress")?;

    let (done_tx, done_rx) = mpsc::channel();

    let handle = std::thread::spawn(move || -> Result<()> {
        for seq in 0..messages as u64 {
            let payload = build_payload(seq);
            writer.append(TYPE_ID, &payload)?;
            if seq % 4096 == 0 {
                writer.cleanup()?;
            }
        }
        writer.flush_sync()?;
        let _ = done_tx.send(());
        Ok(())
    });

    let mut expected_seq: u64 = 0;
    let mut last_progress = Instant::now();
    while expected_seq < messages as u64 {
        if let Some(message) = reader.next()? {
            validate_message(&message, expected_seq)?;
            expected_seq += 1;
            last_progress = Instant::now();
            if expected_seq % 4096 == 0 {
                reader.commit()?;
            }
        } else {
            reader.wait(Some(Duration::from_millis(5)))?;
            if last_progress.elapsed() > max_idle {
                return Err(Error::Corrupt("stress reader stalled"));
            }
        }
    }

    reader.commit()?;
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| Error::Corrupt("writer did not finish"))?;
    handle.join().expect("writer thread")?;

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

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}
