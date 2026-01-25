use chronicle::core::merge::FanInReader;
use chronicle::core::{Queue, WriterConfig};
use tempfile::tempdir;

const TEST_SEGMENT_SIZE: usize = 1 * 1024 * 1024;

#[test]
fn merge_orders_by_timestamp_and_source() {
    let dir = tempdir().expect("tempdir");
    let queue_a_path = dir.path().join("queue_a");
    let queue_b_path = dir.path().join("queue_b");

    let mut writer_a = Queue::open_publisher_with_config(
        &queue_a_path,
        WriterConfig {
            segment_size_bytes: TEST_SEGMENT_SIZE as u64,
            ..WriterConfig::default()
        },
    )
    .expect("queue a");
    let mut writer_b = Queue::open_publisher_with_config(
        &queue_b_path,
        WriterConfig {
            segment_size_bytes: TEST_SEGMENT_SIZE as u64,
            ..WriterConfig::default()
        },
    )
    .expect("queue b");

    writer_a
        .append_with_timestamp(1, b"a1", 100)
        .expect("append a1");
    writer_b
        .append_with_timestamp(1, b"b1", 200)
        .expect("append b1");
    writer_a
        .append_with_timestamp(1, b"a2", 300)
        .expect("append a2");
    writer_b
        .append_with_timestamp(1, b"b2", 300)
        .expect("append b2");

    let reader_a = Queue::open_subscriber(&queue_a_path, "fanin").expect("reader a");
    let reader_b = Queue::open_subscriber(&queue_b_path, "fanin").expect("reader b");

    let mut fanin = FanInReader::new(vec![reader_a, reader_b]);
    let mut seen = Vec::new();

    while let Some(message) = fanin.next().expect("fanin next") {
        seen.push((
            message.source,
            message.timestamp_ns,
            message.payload.to_vec(),
        ));
    }

    assert_eq!(
        seen,
        vec![
            (0, 100, b"a1".to_vec()),
            (1, 200, b"b1".to_vec()),
            (0, 300, b"a2".to_vec()),
            (1, 300, b"b2".to_vec()),
        ]
    );
}

#[test]
fn merge_returns_none_when_empty() {
    let dir = tempdir().expect("tempdir");
    let queue_a_path = dir.path().join("queue_a");
    let queue_b_path = dir.path().join("queue_b");

    let reader_a = Queue::open_subscriber(&queue_a_path, "fanin");
    assert!(reader_a.is_err());

    let _writer_a = Queue::open_publisher_with_config(
        &queue_a_path,
        WriterConfig {
            segment_size_bytes: TEST_SEGMENT_SIZE as u64,
            ..WriterConfig::default()
        },
    )
    .expect("writer a");
    let _writer_b = Queue::open_publisher_with_config(
        &queue_b_path,
        WriterConfig {
            segment_size_bytes: TEST_SEGMENT_SIZE as u64,
            ..WriterConfig::default()
        },
    )
    .expect("writer b");

    let reader_a = Queue::open_subscriber(&queue_a_path, "fanin").expect("reader a");
    let reader_b = Queue::open_subscriber(&queue_b_path, "fanin").expect("reader b");

    let mut fanin = FanInReader::new(vec![reader_a, reader_b]);
    let message = fanin.next().expect("fanin next");
    assert!(message.is_none());
}
