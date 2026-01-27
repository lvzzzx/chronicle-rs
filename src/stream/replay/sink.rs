use crate::core::{Error, Result, Queue, QueueWriter, WriterConfig};
use crate::stream::StreamMessageRef;
use std::fs::File;
use std::path::Path;

/// Sink trait for message outputs.
pub trait Sink: Send {
    /// Write a message to the sink.
    fn write(&mut self, msg: &StreamMessageRef<'_>) -> Result<()>;

    /// Flush any buffered data.
    fn flush(&mut self) -> Result<()>;
}

/// Write messages to a Chronicle queue.
pub struct QueueSink {
    writer: QueueWriter,
}

impl QueueSink {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let writer = Queue::open_publisher(path)?;
        Ok(Self { writer })
    }

    pub fn open_with_config(path: impl AsRef<Path>, config: WriterConfig) -> Result<Self> {
        let writer = Queue::open_publisher_with_config(path, config)?;
        Ok(Self { writer })
    }
}

impl Sink for QueueSink {
    fn write(&mut self, msg: &StreamMessageRef<'_>) -> Result<()> {
        self.writer.append(msg.type_id, msg.payload)?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush_sync()
    }
}

/// Write messages to CSV.
#[cfg(feature = "stream")]
pub struct CsvSink {
    writer: csv::Writer<File>,
}

#[cfg(feature = "stream")]
impl CsvSink {
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::create(path).map_err(Error::Io)?;
        let mut writer = csv::WriterBuilder::new()
            .has_headers(true)
            .from_writer(file);

        // Write header
        writer.write_record(&["seq", "timestamp_ns", "type_id", "payload_len"])
            .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

        Ok(Self { writer })
    }
}

#[cfg(feature = "stream")]
impl Sink for CsvSink {
    fn write(&mut self, msg: &StreamMessageRef<'_>) -> Result<()> {
        self.writer
            .write_record(&[
                msg.seq.to_string(),
                msg.timestamp_ns.to_string(),
                msg.type_id.to_string(),
                msg.payload.len().to_string(),
            ])
            .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush().map_err(Error::Io)
    }
}

/// Sink that drops all messages (for benchmarking).
pub struct NullSink;

impl Sink for NullSink {
    fn write(&mut self, _msg: &StreamMessageRef<'_>) -> Result<()> {
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Sink that collects messages in memory.
pub struct VecSink {
    messages: Vec<(u64, u64, u16, Vec<u8>)>, // (seq, timestamp, type_id, payload)
}

impl VecSink {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    pub fn into_messages(self) -> Vec<(u64, u64, u16, Vec<u8>)> {
        self.messages
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

impl Default for VecSink {
    fn default() -> Self {
        Self::new()
    }
}

impl Sink for VecSink {
    fn write(&mut self, msg: &StreamMessageRef<'_>) -> Result<()> {
        self.messages.push((
            msg.seq,
            msg.timestamp_ns,
            msg.type_id,
            msg.payload.to_vec(),
        ));
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}
