use chronicle::core::header::{MessageHeader, HEADER_SIZE, RECORD_ALIGN};
use chronicle::core::mmap::MmapFile;
use chronicle::core::segment::SEG_DATA_OFFSET;
use chronicle::core::{Queue, WriterConfig};
use tempfile::tempdir;

const TEST_SEGMENT_SIZE: usize = 1 * 1024 * 1024;

fn read_header(map: &[u8], offset: usize) -> MessageHeader {
    let mut buf = [0u8; 64];
    buf.copy_from_slice(&map[offset..offset + 64]);
    MessageHeader::from_bytes(&buf).expect("header parse")
}

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

#[test]
fn append_two_messages_and_recover() {
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

    let payload_a = b"alpha";
    let payload_b = b"bravo-bravo";

    writer.append(1, payload_a).expect("append alpha");
    writer.append(1, payload_b).expect("append bravo");
    writer.flush_sync().expect("flush index");

    let segment_path = queue_path.join("000000000.q");
    let mmap = MmapFile::open(&segment_path).expect("mmap open");
    assert_eq!(mmap.len(), TEST_SEGMENT_SIZE);

    let first_offset = SEG_DATA_OFFSET;
    let first_len = align_up(HEADER_SIZE + payload_a.len(), RECORD_ALIGN);
    let second_offset = first_offset + first_len;
    let second_len = align_up(HEADER_SIZE + payload_b.len(), RECORD_ALIGN);

    let header_a = read_header(mmap.as_slice(), first_offset);
    let commit_a = MessageHeader::load_commit_len(&mmap.as_slice()[first_offset] as *const u8);
    assert!(commit_a > 0);
    header_a
        .validate_crc(&mmap.as_slice()[first_offset + HEADER_SIZE..first_offset + HEADER_SIZE + payload_a.len()])
        .expect("crc alpha");

    let header_b = read_header(mmap.as_slice(), second_offset);
    let commit_b = MessageHeader::load_commit_len(&mmap.as_slice()[second_offset] as *const u8);
    assert!(commit_b > 0);
    header_b
        .validate_crc(&mmap.as_slice()[second_offset + HEADER_SIZE..second_offset + HEADER_SIZE + payload_b.len()])
        .expect("crc bravo");

    drop(writer);

    let mut writer = Queue::open_publisher_with_config(
        &queue_path,
        WriterConfig {
            segment_size_bytes: TEST_SEGMENT_SIZE as u64,
            ..WriterConfig::default()
        },
    )
    .expect("queue reopen");
    let payload_c = b"charlie";
    writer.append(1, payload_c).expect("append charlie");

    let mmap = MmapFile::open(&segment_path).expect("mmap open");
    let third_offset = second_offset + second_len;
    let header_c = read_header(mmap.as_slice(), third_offset);
    let commit_c = MessageHeader::load_commit_len(&mmap.as_slice()[third_offset] as *const u8);
    assert!(commit_c > 0);
    header_c
        .validate_crc(&mmap.as_slice()[third_offset + HEADER_SIZE..third_offset + HEADER_SIZE + payload_c.len()])
        .expect("crc charlie");
}
