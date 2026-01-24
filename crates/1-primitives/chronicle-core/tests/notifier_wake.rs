#[cfg(target_os = "linux")]
use chronicle_core::{Queue, WriterConfig};
#[cfg(target_os = "linux")]
use std::sync::mpsc;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
#[test]
fn reader_wait_wakes_on_append() -> chronicle_core::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut writer = Queue::open_publisher_with_config(
        dir.path(),
        WriterConfig {
            segment_size_bytes: 1 * 1024 * 1024,
            ..WriterConfig::default()
        },
    )?;
    let mut reader = Queue::open_subscriber(dir.path(), "reader_a")?;

    assert!(reader.next()?.is_none());

    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();

    let handle = std::thread::spawn(move || -> chronicle_core::Result<()> {
        let _ = started_tx.send(());
        reader.wait(Some(Duration::from_secs(1)))?;
        let _ = done_tx.send(());
        Ok(())
    });

    started_rx.recv().unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());

    writer.append(1, b"ping")?;
    done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    handle.join().unwrap()?;

    Ok(())
}
