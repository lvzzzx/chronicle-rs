use chronicle::Queue;
use tempfile::tempdir;

#[test]
fn read_commit_and_recover() {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("orders");

    let mut writer = Queue::open_publisher(&queue_path).expect("queue open");

    let payload_a = b"alpha";
    let payload_b = b"bravo";
    writer.append(1, payload_a).expect("append alpha");
    writer.append(1, payload_b).expect("append bravo");

    let mut reader = Queue::open_subscriber(&queue_path, "engine").expect("reader open");
    let msg_a = reader.next().expect("read a").expect("msg a");
    assert_eq!(msg_a.payload, payload_a);
    reader.commit().expect("commit a");

    let msg_b = reader.next().expect("read b").expect("msg b");
    assert_eq!(msg_b.payload, payload_b);
    reader.commit().expect("commit b");

    assert!(reader.next().expect("read none").is_none());
    drop(reader);

    let mut reader = Queue::open_subscriber(&queue_path, "engine").expect("reader reopen");
    assert!(reader.next().expect("read none after reopen").is_none());
}
