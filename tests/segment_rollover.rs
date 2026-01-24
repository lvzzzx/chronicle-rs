use chronicle::core::header::HEADER_SIZE;
use chronicle::core::segment::SEG_DATA_OFFSET;
use chronicle::core::{Queue, WriterConfig};
use tempfile::tempdir;

const TEST_SEGMENT_SIZE: usize = 1 * 1024 * 1024;

#[test]
fn segment_rollover_and_read_across() {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("orders");

    let mut writer = Queue::open_publisher_with_config(
        &queue_path,
        WriterConfig {
            segment_size_bytes: TEST_SEGMENT_SIZE as u64,
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

    let segment0 = queue_path.join("000000000.q");
    let segment1 = queue_path.join("000000001.q");
    assert!(segment0.exists());
    assert!(segment1.exists());

    let mut reader = Queue::open_subscriber(&queue_path, "engine").expect("reader open");
    let msg_a = reader.next().expect("read a").expect("msg a");
    assert_eq!(msg_a.payload, payload_a);
    reader.commit().expect("commit a");

    let msg_b = reader.next().expect("read b").expect("msg b");
    assert_eq!(msg_b.payload, payload_b);
    reader.commit().expect("commit b");

    assert!(reader.next().expect("read none").is_none());
}
