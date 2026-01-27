use std::path::Path;
use std::time::Duration;

use crate::core::WaitStrategy;
use crate::storage::access::{StorageReader, StorageResolver};
use anyhow::Result;

use super::{OwnedStreamReader, StreamMessageOwned, StreamMessageRef, StreamReader};

pub struct ArchiveStream {
    reader: StorageReader,
}

impl ArchiveStream {
    pub fn open(
        resolver: &StorageResolver,
        venue: &str,
        symbol_code: &str,
        date: &str,
        stream: &str,
    ) -> Result<Self> {
        let reader = StorageReader::open(resolver, venue, symbol_code, date, stream)?;
        Ok(Self { reader })
    }

    pub fn open_segment(path: impl AsRef<Path>) -> Result<Self> {
        let reader = StorageReader::open_segment(path)?;
        Ok(Self { reader })
    }

    pub fn reader(&self) -> &StorageReader {
        &self.reader
    }

    pub fn reader_mut(&mut self) -> &mut StorageReader {
        &mut self.reader
    }

    pub fn into_inner(self) -> StorageReader {
        self.reader
    }
}

impl StreamReader for ArchiveStream {
    fn next<'a>(&'a mut self) -> Result<Option<StreamMessageRef<'a>>> {
        let msg = self.reader.next()?;
        Ok(msg.map(StreamMessageRef::from))
    }

    fn wait(&mut self, _timeout: Option<Duration>) -> Result<()> {
        Ok(())
    }

    fn commit(&mut self) -> Result<()> {
        Ok(())
    }

    fn set_wait_strategy(&mut self, _strategy: WaitStrategy) {}

    fn seek_timestamp(&mut self, target_ts_ns: u64) -> Result<bool> {
        self.reader.seek_timestamp(target_ts_ns)
    }
}

impl OwnedStreamReader for ArchiveStream {
    fn next_owned(&mut self) -> Result<Option<StreamMessageOwned>> {
        let msg = self.reader.next()?;
        Ok(msg.map(|view| StreamMessageOwned {
            seq: view.seq,
            timestamp_ns: view.timestamp_ns,
            type_id: view.type_id,
            payload: view.payload.to_vec(),
        }))
    }

    fn wait(&mut self, _timeout: Option<Duration>) -> Result<()> {
        Ok(())
    }

    fn commit(&mut self) -> Result<()> {
        Ok(())
    }

    fn set_wait_strategy(&mut self, _strategy: WaitStrategy) {}
}
