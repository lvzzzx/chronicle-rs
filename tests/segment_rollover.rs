use chronicle::segment::SEGMENT_SIZE;
use chronicle::Queue;
use tempfile::tempdir;

const HEADER_SIZE: usize = 64;

#[test]
fn segment_rollover_and_read_across() {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("orders");

    let queue = Queue::open(&queue_path).expect("queue open");
    let writer = queue.writer();

    let payload_a = vec![b'a'; SEGMENT_SIZE - HEADER_SIZE];
    let payload_b = b"bravo".to_vec();

    writer.append(&payload_a).expect("append alpha");
    writer.append(&payload_b).expect("append bravo");
    writer.flush().expect("flush index");

    let segment0 = queue_path.join("000000000.q");
    let segment1 = queue_path.join("000000001.q");
    assert!(segment0.exists());
    assert!(segment1.exists());

    let mut reader = queue.reader("engine").expect("reader open");
    let msg_a = reader.next().expect("read a").expect("msg a");
    assert_eq!(msg_a, payload_a);
    reader.commit().expect("commit a");

    let msg_b = reader.next().expect("read b").expect("msg b");
    assert_eq!(msg_b, payload_b);
    reader.commit().expect("commit b");

    assert!(reader.next().expect("read none").is_none());
}
