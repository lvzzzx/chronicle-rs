use chronicle::merge::FanInReader;
use chronicle::Queue;
use tempfile::tempdir;

#[test]
fn merge_orders_by_timestamp_and_source() {
    let dir = tempdir().expect("tempdir");
    let queue_a_path = dir.path().join("queue_a");
    let queue_b_path = dir.path().join("queue_b");

    let queue_a = Queue::open(&queue_a_path).expect("queue a");
    let queue_b = Queue::open(&queue_b_path).expect("queue b");

    let writer_a = queue_a.writer();
    let writer_b = queue_b.writer();

    writer_a
        .append_with_timestamp(b"a1", 100)
        .expect("append a1");
    writer_b
        .append_with_timestamp(b"b1", 200)
        .expect("append b1");
    writer_a
        .append_with_timestamp(b"a2", 300)
        .expect("append a2");
    writer_b
        .append_with_timestamp(b"b2", 300)
        .expect("append b2");

    let reader_a = queue_a.reader("fanin").expect("reader a");
    let reader_b = queue_b.reader("fanin").expect("reader b");

    let mut fanin = FanInReader::new(vec![reader_a, reader_b]);
    let mut seen = Vec::new();

    while let Some(message) = fanin.next().expect("fanin next") {
        seen.push((
            message.source,
            message.header.timestamp_ns,
            message.payload,
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

    let queue_a = Queue::open(&queue_a_path).expect("queue a");
    let queue_b = Queue::open(&queue_b_path).expect("queue b");

    let reader_a = queue_a.reader("fanin").expect("reader a");
    let reader_b = queue_b.reader("fanin").expect("reader b");

    let mut fanin = FanInReader::new(vec![reader_a, reader_b]);
    let message = fanin.next().expect("fanin next");
    assert!(message.is_none());
}
