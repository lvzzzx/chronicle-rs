use chronicle::segment::SEGMENT_SIZE;
use chronicle::Queue;
use tempfile::tempdir;

const HEADER_SIZE: usize = 64;

#[test]
fn retention_deletes_only_when_all_readers_advance() {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("orders");

    let queue = Queue::open(&queue_path).expect("queue open");
    let writer = queue.writer();

    let payload_a = vec![b'a'; SEGMENT_SIZE - HEADER_SIZE];
    let payload_b = b"bravo".to_vec();

    writer.append(&payload_a).expect("append alpha");
    writer.append(&payload_b).expect("append bravo");
    writer.flush().expect("flush index");

    let mut reader_fast = queue.reader("fast").expect("reader fast");
    let msg_a = reader_fast.next().expect("read a").expect("msg a");
    assert_eq!(msg_a.len(), payload_a.len());
    reader_fast.commit().expect("commit a");
    let msg_b = reader_fast.next().expect("read b").expect("msg b");
    assert_eq!(msg_b, payload_b);
    reader_fast.commit().expect("commit b");

    let mut reader_slow = queue.reader("slow").expect("reader slow");
    let msg_a2 = reader_slow.next().expect("read a2").expect("msg a2");
    assert_eq!(msg_a2.len(), payload_a.len());
    reader_slow.commit().expect("commit a2");

    let segment0 = queue_path.join("000000000.q");
    let deleted = queue.cleanup().expect("cleanup");
    assert!(deleted.is_empty());
    assert!(segment0.exists());

    let msg_b2 = reader_slow.next().expect("read b2").expect("msg b2");
    assert_eq!(msg_b2, payload_b);
    reader_slow.commit().expect("commit b2");

    let deleted = queue.cleanup().expect("cleanup after advance");
    assert_eq!(deleted, vec![0]);
    assert!(!segment0.exists());
}
