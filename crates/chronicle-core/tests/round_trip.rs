use chronicle_core::header::MessageHeader;
use chronicle_core::mmap::MmapFile;
use tempfile::tempdir;

#[test]
fn append_read_round_trip() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("queue.q");

    let payload = b"hello world";
    let checksum = MessageHeader::crc32(payload);
    let header = MessageHeader::new_uncommitted(1, 42, 7, 0, checksum);

    let mut mmap = MmapFile::create(&path, 1024).expect("mmap create");

    // Write header with commit_len = 0, then payload, then publish commit_len.
    let header_bytes = header.to_bytes();
    mmap.range_mut(0, 64)
        .expect("header range")
        .copy_from_slice(&header_bytes);
    mmap.range_mut(64, payload.len())
        .expect("payload range")
        .copy_from_slice(payload);

    let commit_len = MessageHeader::commit_len_for_payload(payload.len()).expect("commit len");
    let header_ptr = mmap.as_mut_slice().as_mut_ptr();
    MessageHeader::store_commit_len(header_ptr, commit_len);

    drop(mmap);

    let mmap = MmapFile::open(&path).expect("mmap open");
    let mut header_buf = [0u8; 64];
    header_buf.copy_from_slice(&mmap.as_slice()[0..64]);
    let read_header = MessageHeader::from_bytes(&header_buf).expect("header parse");
    let commit = MessageHeader::load_commit_len(&mmap.as_slice()[0] as *const u8);
    assert!(commit > 0);
    let read_payload = &mmap.as_slice()[64..64 + payload.len()];
    assert_eq!(read_payload, payload);
    read_header.validate_crc(read_payload).expect("crc ok");
}
