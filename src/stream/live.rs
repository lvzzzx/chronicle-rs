use std::path::Path;
use std::time::Duration;

use crate::core::{MessageView, Queue, QueueReader, ReaderConfig, WaitStrategy};
use anyhow::Result;

use super::{OwnedStreamReader, StreamMessageOwned, StreamMessageRef, StreamReader};

pub struct LiveStream {
    reader: QueueReader,
}

impl LiveStream {
    pub fn open(path: impl AsRef<Path>, reader_name: &str) -> Result<Self> {
        let reader = Queue::open_subscriber(path.as_ref(), reader_name)?;
        Ok(Self { reader })
    }

    pub fn open_with_config(
        path: impl AsRef<Path>,
        reader_name: &str,
        config: ReaderConfig,
    ) -> Result<Self> {
        let reader = Queue::open_subscriber_with_config(path.as_ref(), reader_name, config)?;
        Ok(Self { reader })
    }

    pub fn from_reader(reader: QueueReader) -> Self {
        Self { reader }
    }

    pub fn reader(&self) -> &QueueReader {
        &self.reader
    }

    pub fn reader_mut(&mut self) -> &mut QueueReader {
        &mut self.reader
    }

    pub fn into_inner(self) -> QueueReader {
        self.reader
    }
}

impl StreamReader for LiveStream {
    fn next<'a>(&'a mut self) -> Result<Option<StreamMessageRef<'a>>> {
        let msg = self.reader.next()?;
        Ok(msg.map(StreamMessageRef::from))
    }

    fn wait(&mut self, timeout: Option<Duration>) -> Result<()> {
        Ok(self.reader.wait(timeout)?)
    }

    fn commit(&mut self) -> Result<()> {
        Ok(self.reader.commit()?)
    }

    fn set_wait_strategy(&mut self, strategy: WaitStrategy) {
        self.reader.set_wait_strategy(strategy);
    }

    fn seek_seq(&mut self, seq: u64) -> Result<bool> {
        Ok(self.reader.seek_seq(seq)?)
    }

    fn seek_timestamp(&mut self, target_ts_ns: u64) -> Result<bool> {
        Ok(self.reader.seek_timestamp(target_ts_ns)?)
    }
}

impl OwnedStreamReader for LiveStream {
    fn next_owned(&mut self) -> Result<Option<StreamMessageOwned>> {
        let msg = self.reader.next()?;
        Ok(msg.map(|view: MessageView<'_>| StreamMessageOwned {
            seq: view.seq,
            timestamp_ns: view.timestamp_ns,
            type_id: view.type_id,
            payload: view.payload.to_vec(),
        }))
    }

    fn wait(&mut self, timeout: Option<Duration>) -> Result<()> {
        Ok(self.reader.wait(timeout)?)
    }

    fn commit(&mut self) -> Result<()> {
        Ok(self.reader.commit()?)
    }

    fn set_wait_strategy(&mut self, strategy: WaitStrategy) {
        self.reader.set_wait_strategy(strategy);
    }
}
