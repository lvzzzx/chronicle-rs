use std::mem::size_of;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chronicle::core::control::ControlBlock;
use chronicle::core::mmap::MmapFile;
use chronicle::core::segment::segment_path;
use chronicle::core::{DisconnectReason, Queue, WriterConfig};
use tempfile::tempdir;

fn now_ns() -> u64 {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time");
    u64::try_from(ts.as_nanos()).expect("timestamp range")
}

#[test]
fn try_open_subscriber_missing_control_returns_none() {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("missing-control");

    let res = Queue::try_open_subscriber(&queue_path, "reader").expect("try open");
    assert!(res.is_none());
}

#[test]
fn try_open_subscriber_not_ready_returns_none() {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("not-ready");
    std::fs::create_dir_all(&queue_path).expect("mkdir");

    let control_path = queue_path.join("control.meta");
    let mut mmap = MmapFile::create(&control_path, size_of::<ControlBlock>()).expect("mmap");
    mmap.as_mut_slice().fill(0);
    mmap.flush_sync().expect("flush");
    drop(mmap);

    let res = Queue::try_open_subscriber(&queue_path, "reader").expect("try open");
    assert!(res.is_none());
}

#[test]
fn detect_disconnect_segment_missing() {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("segment-missing");

    let _writer = Queue::open_publisher_with_config(
        &queue_path,
        WriterConfig {
            segment_size_bytes: 4096,
            ..WriterConfig::default()
        },
    )
    .expect("writer");

    let reader = Queue::open_subscriber(&queue_path, "reader").expect("reader");
    let seg_path = segment_path(&queue_path, reader.segment_id() as u64);
    std::fs::remove_file(&seg_path).expect("remove segment");

    let reason = reader
        .detect_disconnect(Duration::from_secs(1))
        .expect("detect");
    assert_eq!(reason, Some(DisconnectReason::SegmentMissing));
}

#[test]
fn detect_disconnect_heartbeat_stale() {
    let dir = tempdir().expect("tempdir");
    let queue_path = dir.path().join("heartbeat-stale");

    let _writer = Queue::open_publisher_with_config(
        &queue_path,
        WriterConfig {
            segment_size_bytes: 4096,
            ..WriterConfig::default()
        },
    )
    .expect("writer");

    let reader = Queue::open_subscriber(&queue_path, "reader").expect("reader");

    let control_path = queue_path.join("control.meta");
    let control = chronicle::core::control::ControlFile::open(&control_path).expect("control");
    let stale = now_ns().saturating_sub(10_000_000);
    control.set_writer_heartbeat_ns(stale);

    let reason = reader
        .detect_disconnect(Duration::from_millis(1))
        .expect("detect");
    assert_eq!(reason, Some(DisconnectReason::HeartbeatStale));
}
