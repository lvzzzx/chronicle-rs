use chronicle_core::{Queue, ReaderConfig, StartMode, WriterConfig};

#[test]
fn seek_seq_and_timestamp() -> chronicle_core::Result<()> {
    let dir = tempfile::tempdir().map_err(chronicle_core::Error::Io)?;
    let config = WriterConfig {
        segment_size_bytes: 4096,
        ..WriterConfig::default()
    };
    let mut writer = Queue::open_publisher_with_config(dir.path(), config)?;
    let total = 80_u64;
    for i in 0..total {
        let timestamp_ns = 1_000_000 + i;
        writer.append_with_timestamp(1, &[], timestamp_ns)?;
    }
    writer.flush_sync()?;
    drop(writer);

    let mut reader = Queue::open_subscriber_with_config(
        dir.path(),
        "seek_test",
        ReaderConfig {
            start_mode: StartMode::Earliest,
            memlock: false,
        },
    )?;

    assert!(reader.seek_seq(10)?);
    let msg = reader.next()?.expect("missing message after seek_seq");
    assert_eq!(msg.seq, 10);

    assert!(reader.seek_timestamp(1_000_020)?);
    let msg = reader.next()?.expect("missing message after seek_timestamp");
    assert_eq!(msg.seq, 20);

    Ok(())
}
