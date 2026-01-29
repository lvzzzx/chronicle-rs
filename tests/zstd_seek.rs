use std::fs::File;
use std::io::Write;

use chronicle::core::compression::read_seek_index;
use chronicle::core::compression::{compress_q_to_zst, ZstdBlockReader};
use tempfile::tempdir;

#[test]
fn zstd_seek_index_roundtrip() {
    let dir = tempdir().expect("tempdir");
    let input_path = dir.path().join("input.q");
    let zst_path = dir.path().join("output.q.zst");
    let idx_path = dir.path().join("output.q.zst.idx");

    let mut input = File::create(&input_path).expect("create input");
    let data: Vec<u8> = (0..512).map(|i| (i % 251) as u8).collect();
    input.write_all(&data).expect("write input");
    drop(input);

    let block_size = 64usize;
    compress_q_to_zst(&input_path, &zst_path, &idx_path, block_size).expect("compress");

    let index = read_seek_index(&idx_path).expect("read index");
    let expected_entries = (data.len() + block_size - 1) / block_size;
    assert_eq!(index.entries.len(), expected_entries);

    let mut reader = ZstdBlockReader::open(&zst_path, &idx_path).expect("open reader");
    for offset in [0usize, 63, 64, 127, 255, 511] {
        let entry = index.entry_for_offset(offset as u64).expect("entry");
        let block = reader.read_block_at(offset as u64).expect("read block");
        let start = entry.uncompressed_offset as usize;
        let end = start + entry.uncompressed_size as usize;
        assert_eq!(&block[..], &data[start..end]);
    }
}
