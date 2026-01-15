use chronicle::header::MessageHeader;
use chronicle::mmap::MmapFile;
use tempfile::tempdir;

#[test]
fn append_read_round_trip() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("queue.q");

    let payload = b"hello world";
    let checksum = MessageHeader::crc32(payload);
    let mut header = MessageHeader::new(payload.len() as u32, 1, 42, 7, checksum);

    let mut mmap = MmapFile::create(&path, 1024).expect("mmap create");

    // Write header with flags = 0, then payload, then commit flag.
    let header_bytes = header.to_bytes();
    mmap.range_mut(0, 64)
        .expect("header range")
        .copy_from_slice(&header_bytes);
    mmap.range_mut(64, payload.len())
        .expect("payload range")
        .copy_from_slice(payload);

    header.flags = 1;
    let committed = header.to_bytes();
    mmap.range_mut(0, 64)
        .expect("header range")
        .copy_from_slice(&committed);

    drop(mmap);

    let mmap = MmapFile::open(&path).expect("mmap open");
    let mut header_buf = [0u8; 64];
    header_buf.copy_from_slice(&mmap.as_slice()[0..64]);
    let read_header = MessageHeader::from_bytes(&header_buf).expect("header parse");

    assert_eq!(read_header.flags, 1);
    let read_payload = &mmap.as_slice()[64..64 + payload.len()];
    assert_eq!(read_payload, payload);
    read_header.validate_crc(read_payload).expect("crc ok");
}
