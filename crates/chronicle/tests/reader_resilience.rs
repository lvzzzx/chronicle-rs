use chronicle::core::{Queue, ReaderConfig, StartMode, WriterConfig};
use tempfile::tempdir;
use std::fs;

const TEST_SEGMENT_SIZE: usize = 4096; // Small segments for easy rolling

#[test]
fn test_resume_snapshot_skips_missing() {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("market");

    let mut writer = Queue::open_publisher_with_config(
        &queue_path,
        WriterConfig {
            segment_size_bytes: TEST_SEGMENT_SIZE as u64,
            ..WriterConfig::default()
        },
    )
    .expect("queue open");

    // Write enough to fill Seg 0 and start Seg 1
    // Header is 64 bytes. Payload "msg" is 3. Padded to 64. 
    // Segment size 4096. 4096 / 64 = 64 messages.
    // Let's write 100 messages.
    for _ in 0..100 {
        writer.append(1, b"msg").expect("append");
    }
    
    let mut reader = Queue::open_subscriber(&queue_path, "resilient_reader").expect("reader open");
    // Read 10 messages
    for _ in 0..10 {
        assert!(reader.next().expect("read").is_some());
    }
    reader.commit().expect("commit");
    drop(reader); // Close reader

    // Manually delete Segment 0 (simulation of retention)
    // Segment 0 is "000000000.q"
    let seg0 = queue_path.join("000000000.q");
    fs::remove_file(&seg0).expect("delete seg 0");

    // Case 1: ResumeStrict (Default) -> Should Fail
    let err = Queue::open_subscriber(&queue_path, "resilient_reader");
    assert!(err.is_err());

    // Case 2: ResumeSnapshot -> Should Succeed and skip to Seg 1
    let config = ReaderConfig {
        start_mode: StartMode::ResumeSnapshot,
        ..ReaderConfig::default()
    };
    let mut reader = Queue::open_subscriber_with_config(&queue_path, "resilient_reader", config)
        .expect("reader open with snapshot");
    
    // We should be on Segment 1 now.
    // Messages 0-63 (approx) were in Seg 0. Messages 64+ in Seg 1.
    // Since we committed 10, we missed 11-63. That's fine, we "snapped".
    // Next message should be valid.
    let msg = reader.next().expect("read").expect("msg");
    assert!(msg.seq > 10); 
    assert_eq!(reader.segment_id(), 1);
}

#[test]
fn test_start_latest() {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("signals");

    let mut writer = Queue::open_publisher_with_config(
        &queue_path,
        WriterConfig {
            segment_size_bytes: TEST_SEGMENT_SIZE as u64,
            ..WriterConfig::default()
        },
    )
    .expect("queue open");

    writer.append(1, b"old").expect("append old");

    // Open reader at Latest
    let config = ReaderConfig {
        start_mode: StartMode::Latest,
        ..ReaderConfig::default()
    };
    let mut reader = Queue::open_subscriber_with_config(&queue_path, "late_reader", config)
        .expect("reader open latest");

    // Should see nothing yet
    assert!(reader.next().expect("read").is_none());

    // Write new
    writer.append(1, b"new").expect("append new");

    // Should see new
    let msg = reader.next().expect("read").expect("msg");
    assert_eq!(msg.payload, b"new");
}
