use std::time::Duration;

use crate::core::WaitStrategy;
use anyhow::Result;

pub mod etl;
pub mod live;
pub mod merge;
pub mod replay;
pub mod reconstruct;
#[cfg(feature = "storage")]
pub mod archive;

pub use live::LiveStream;
#[cfg(feature = "storage")]
pub use archive::ArchiveStream;

#[derive(Debug, Clone, Copy)]
pub struct StreamMessageRef<'a> {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub type_id: u16,
    pub payload: &'a [u8],
}

#[derive(Debug, Clone)]
pub struct StreamMessageOwned {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub type_id: u16,
    pub payload: Vec<u8>,
}

pub trait StreamMessageView<'a> {
    fn seq(&self) -> u64;
    fn timestamp_ns(&self) -> u64;
    fn type_id(&self) -> u16;
    fn payload(&self) -> &'a [u8];
}

impl<'a> StreamMessageView<'a> for StreamMessageRef<'a> {
    fn seq(&self) -> u64 {
        self.seq
    }

    fn timestamp_ns(&self) -> u64 {
        self.timestamp_ns
    }

    fn type_id(&self) -> u16 {
        self.type_id
    }

    fn payload(&self) -> &'a [u8] {
        self.payload
    }
}

impl<'a> StreamMessageView<'a> for crate::core::MessageView<'a> {
    fn seq(&self) -> u64 {
        self.seq
    }

    fn timestamp_ns(&self) -> u64 {
        self.timestamp_ns
    }

    fn type_id(&self) -> u16 {
        self.type_id
    }

    fn payload(&self) -> &'a [u8] {
        self.payload
    }
}

impl<'a> From<crate::core::MessageView<'a>> for StreamMessageRef<'a> {
    fn from(msg: crate::core::MessageView<'a>) -> Self {
        Self {
            seq: msg.seq,
            timestamp_ns: msg.timestamp_ns,
            type_id: msg.type_id,
            payload: msg.payload,
        }
    }
}

impl StreamMessageOwned {
    pub fn from_view<'a>(view: &impl StreamMessageView<'a>) -> Self {
        Self {
            seq: view.seq(),
            timestamp_ns: view.timestamp_ns(),
            type_id: view.type_id(),
            payload: view.payload().to_vec(),
        }
    }
}

pub trait StreamReader {
    fn next<'a>(&'a mut self) -> Result<Option<StreamMessageRef<'a>>>;
    fn wait(&mut self, timeout: Option<Duration>) -> Result<()>;
    fn commit(&mut self) -> Result<()>;
    fn set_wait_strategy(&mut self, strategy: WaitStrategy);

    fn seek_seq(&mut self, _seq: u64) -> Result<bool> {
        Ok(false)
    }

    fn seek_timestamp(&mut self, _target_ts_ns: u64) -> Result<bool> {
        Ok(false)
    }
}

pub trait OwnedStreamReader {
    fn next_owned(&mut self) -> Result<Option<StreamMessageOwned>>;
    fn wait(&mut self, timeout: Option<Duration>) -> Result<()>;
    fn commit(&mut self) -> Result<()>;
    fn set_wait_strategy(&mut self, strategy: WaitStrategy);
}
