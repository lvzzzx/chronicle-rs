use anyhow::Result;
use tempfile::tempdir;
use time::{Date, Month, Time};

use chronicle::core::{Queue, WriterConfig};
use chronicle::layout::RawArchiveLayout;
use chronicle::storage::{RawArchiver, RawArchiverConfig};

#[test]
fn raw_archiver_copies_sealed_segment() -> Result<()> {
    let temp = tempdir()?;
    let queue_dir = temp.path().join("queue");

    let mut config = WriterConfig::default();
    config.segment_size_bytes = 4096;
    let mut writer = Queue::open_publisher_with_config(&queue_dir, config)?;

    let date = Date::from_calendar_date(2026, Month::January, 24)?;
    let time = Time::from_hms(0, 0, 0)?;
    let timestamp_ns = date.with_time(time).assume_utc().unix_timestamp_nanos() as u64;

    let payload = vec![0u8; 512];
    for _ in 0..32 {
        writer.append_with_timestamp(1, &payload, timestamp_ns)?;
        if writer.segment_id() > 0 {
            break;
        }
    }
    assert!(writer.segment_id() > 0, "writer did not roll segments");
    drop(writer);

    let archive_root = temp.path().join("archive");
    let mut archiver_config = RawArchiverConfig::new(&queue_dir, &archive_root, "binance");
    archiver_config.compress = false;
    archiver_config.retain_q = true;
    archiver_config.reader_name = "raw-archiver-test".to_string();

    let mut archiver = RawArchiver::new(archiver_config);
    archiver.run_once()?;

    let layout = RawArchiveLayout::new(&archive_root);
    let segment_path = layout
        .raw_segment_path("binance", "2026-01-24", 0)
        .expect("raw segment path");
    let meta_path = layout
        .raw_meta_path("binance", "2026-01-24")
        .expect("raw meta path");

    assert!(segment_path.exists());
    assert!(meta_path.exists());

    Ok(())
}
