use chronicle::retention::cleanup_segments;
use chronicle::segment::{create_segment, store_reader_meta, ReaderMeta, SEGMENT_SIZE, SEG_DATA_OFFSET};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::tempdir;

#[test]
fn retention_ignores_lagging_reader() {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("orders");
    std::fs::create_dir_all(&queue_path).expect("queue dir");

    create_segment(&queue_path, 0).expect("segment 0");
    create_segment(&queue_path, 1).expect("segment 1");

    let readers_dir = queue_path.join("readers");
    std::fs::create_dir_all(&readers_dir).expect("readers dir");
    let meta_path = readers_dir.join("lagger.meta");
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos() as u64;
    let mut meta = ReaderMeta::new(0, SEG_DATA_OFFSET as u64, now_ns, 0);
    store_reader_meta(&meta_path, &mut meta).expect("store meta");

    let head_segment = (10_u64 * 1024 * 1024 * 1024 / SEGMENT_SIZE as u64) + 2;
    let deleted = cleanup_segments(&queue_path, head_segment, 0).expect("cleanup");

    assert!(deleted.contains(&0));
    assert!(deleted.contains(&1));
}
