use chronicle_core::header::HEADER_SIZE;
use chronicle_core::segment::SEG_DATA_OFFSET;
use chronicle_core::{Queue, WriterConfig};
use tempfile::tempdir;

const TEST_SEGMENT_SIZE: usize = 1 * 1024 * 1024;

#[test]
fn retention_deletes_only_when_all_readers_advance() {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("orders");

    let mut writer = Queue::open_publisher_with_config(
        &queue_path,
        WriterConfig {
            segment_size_bytes: TEST_SEGMENT_SIZE as u64,
            retention_check_interval: std::time::Duration::from_secs(3600),
            ..WriterConfig::default()
        },
    )
    .expect("queue open");

    let usable = TEST_SEGMENT_SIZE - SEG_DATA_OFFSET;
    let payload_a = vec![b'a'; usable - HEADER_SIZE];
    let payload_b = b"bravo".to_vec();

    writer.append(1, &payload_a).expect("append alpha");
    writer.append(1, &payload_b).expect("append bravo");
    writer.flush_sync().expect("flush index");

    let mut reader_fast = Queue::open_subscriber(&queue_path, "fast").expect("reader fast");
    let msg_a = reader_fast.next().expect("read a").expect("msg a");
    assert_eq!(msg_a.payload.len(), payload_a.len());
    reader_fast.commit().expect("commit a");
    let msg_b = reader_fast.next().expect("read b").expect("msg b");
    assert_eq!(msg_b.payload, payload_b);
    reader_fast.commit().expect("commit b");

    let mut reader_slow = Queue::open_subscriber(&queue_path, "slow").expect("reader slow");
    let msg_a2 = reader_slow.next().expect("read a2").expect("msg a2");
    assert_eq!(msg_a2.payload.len(), payload_a.len());
    reader_slow.commit().expect("commit a2");

    let segment0 = queue_path.join("000000000.q");
    let deleted = writer.cleanup().expect("cleanup");
    assert!(deleted.is_empty());
    assert!(segment0.exists());

    let msg_b2 = reader_slow.next().expect("read b2").expect("msg b2");
    assert_eq!(msg_b2.payload, payload_b);
    reader_slow.commit().expect("commit b2");

    let deleted = writer.cleanup().expect("cleanup after advance");
    assert_eq!(deleted, vec![0]);
    assert!(!segment0.exists());
}
