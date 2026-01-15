use chronicle::header::MessageHeader;
use chronicle::mmap::MmapFile;
use chronicle::segment::SEGMENT_SIZE;
use chronicle::Queue;
use tempfile::tempdir;

const HEADER_SIZE: usize = 64;

fn read_header(map: &[u8], offset: usize) -> MessageHeader {
    let mut buf = [0u8; 64];
    buf.copy_from_slice(&map[offset..offset + 64]);
    MessageHeader::from_bytes(&buf).expect("header parse")
}

#[test]
fn append_two_messages_and_recover() {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("orders");

    let queue = Queue::open(&queue_path).expect("queue open");
    let writer = queue.writer();

    let payload_a = b"alpha";
    let payload_b = b"bravo-bravo";

    writer.append(payload_a).expect("append alpha");
    writer.append(payload_b).expect("append bravo");
    writer.flush().expect("flush index");

    let segment_path = queue_path.join("000000000.q");
    let mmap = MmapFile::open(&segment_path).expect("mmap open");
    assert_eq!(mmap.len(), SEGMENT_SIZE);

    let first_offset = 0usize;
    let second_offset = HEADER_SIZE + payload_a.len();

    let header_a = read_header(mmap.as_slice(), first_offset);
    assert_eq!(header_a.length as usize, payload_a.len());
    assert_eq!(header_a.flags, 1);
    header_a
        .validate_crc(&mmap.as_slice()[first_offset + HEADER_SIZE..second_offset])
        .expect("crc alpha");

    let header_b = read_header(mmap.as_slice(), second_offset);
    let payload_b_end = second_offset + HEADER_SIZE + payload_b.len();
    assert_eq!(header_b.length as usize, payload_b.len());
    assert_eq!(header_b.flags, 1);
    header_b
        .validate_crc(&mmap.as_slice()[second_offset + HEADER_SIZE..payload_b_end])
        .expect("crc bravo");

    drop(queue);

    let queue = Queue::open(&queue_path).expect("queue reopen");
    let writer = queue.writer();
    let payload_c = b"charlie";
    writer.append(payload_c).expect("append charlie");

    let mmap = MmapFile::open(&segment_path).expect("mmap open");
    let third_offset = payload_b_end;
    let header_c = read_header(mmap.as_slice(), third_offset);
    assert_eq!(header_c.length as usize, payload_c.len());
    assert_eq!(header_c.flags, 1);
    header_c
        .validate_crc(&mmap.as_slice()[third_offset + HEADER_SIZE..third_offset + HEADER_SIZE + payload_c.len()])
        .expect("crc charlie");
}
