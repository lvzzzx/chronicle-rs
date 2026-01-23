use chronicle_core::{Queue, WriterConfig};

#[test]
fn index_flush_writes_active_segment_index() -> chronicle_core::Result<()> {
    let dir = tempfile::tempdir().map_err(chronicle_core::Error::Io)?;
    let config = WriterConfig {
        segment_size_bytes: 1 * 1024 * 1024,
        index_flush_records: 1,
        index_flush_interval: std::time::Duration::ZERO,
        ..WriterConfig::default()
    };
    let mut writer = Queue::open_publisher_with_config(dir.path(), config)?;
    writer.append(1, b"ping")?;

    let index_path = dir.path().join("000000000.idx");
    assert!(index_path.exists());
    Ok(())
}
