use std::time::{Duration, Instant};

use crate::core::{Error, Result as CoreResult, WaitStrategy};
use crate::core::reader::{MessageRef, QueueReader};
use anyhow::Result;

use super::{OwnedStreamReader, StreamMessageOwned};

pub struct MergedMessage<'a> {
    pub source: usize,
    pub seq: u64,
    pub timestamp_ns: u64,
    pub type_id: u16,
    pub payload: &'a [u8],
}

pub struct FanInReader {
    readers: Vec<QueueReader>,
    pending: Vec<Option<PendingMessage>>,
    wait_strategy: WaitStrategy,
}

struct PendingMessage {
    seq: u64,
    timestamp_ns: u64,
    type_id: u16,
    payload_offset: usize,
    payload_len: usize,
}

impl FanInReader {
    pub fn new(readers: Vec<QueueReader>) -> Self {
        let pending = readers.iter().map(|_| None).collect();
        Self {
            readers,
            pending,
            wait_strategy: WaitStrategy::SpinThenPark { spin_us: 10 },
        }
    }

    pub fn add_reader(&mut self, reader: QueueReader) {
        self.readers.push(reader);
        self.pending.push(None);
    }

    pub fn set_wait_strategy(&mut self, strategy: WaitStrategy) {
        self.wait_strategy = strategy;
    }

    pub fn wait(&mut self) -> CoreResult<()> {
        match self.wait_strategy {
            WaitStrategy::BusySpin => loop {
                if self.any_committed()? {
                    return Ok(());
                }
                std::hint::spin_loop();
            },
            WaitStrategy::Sleep(duration) => {
                if !self.any_committed()? {
                    std::thread::sleep(duration);
                }
                Ok(())
            }
            WaitStrategy::SpinThenPark { spin_us } => {
                let deadline = Instant::now() + Duration::from_micros(spin_us as u64);
                while Instant::now() < deadline {
                    if self.any_committed()? {
                        return Ok(());
                    }
                    std::hint::spin_loop();
                }
                // Degraded Mode: We cannot park on N futexes without eventfd.
                // Fallback to a short sleep (100us) to avoid 100% CPU burn.
                std::thread::sleep(Duration::from_micros(100));
                Ok(())
            }
        }
    }

    fn any_committed(&self) -> CoreResult<bool> {
        for reader in &self.readers {
            if reader.peek_committed()? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn next(&mut self) -> CoreResult<Option<MergedMessage<'_>>> {
        for (index, reader) in self.readers.iter_mut().enumerate() {
            if self.pending[index].is_none() {
                if let Some(message) = reader.next_ref()? {
                    self.pending[index] = Some(PendingMessage::from_ref(message));
                }
            }
        }

        let mut best: Option<(usize, u64)> = None;
        for (index, pending) in self.pending.iter().enumerate() {
            let Some(message) = pending.as_ref() else {
                continue;
            };
            let timestamp = message.timestamp_ns;
            match best {
                None => best = Some((index, timestamp)),
                Some((best_index, best_timestamp)) => {
                    if timestamp < best_timestamp
                        || (timestamp == best_timestamp && index < best_index)
                    {
                        best = Some((index, timestamp));
                    }
                }
            }
        }

        let Some((source, _)) = best else {
            return Ok(None);
        };
        let message = self.pending[source]
            .take()
            .ok_or(Error::Corrupt("pending message missing"))?;
        let payload =
            self.readers[source].payload_at(message.payload_offset, message.payload_len)?;
        Ok(Some(MergedMessage {
            source,
            seq: message.seq,
            timestamp_ns: message.timestamp_ns,
            type_id: message.type_id,
            payload,
        }))
    }

    pub fn commit(&mut self, source: usize) -> CoreResult<()> {
        let reader = self
            .readers
            .get_mut(source)
            .ok_or(Error::Unsupported("invalid fan-in source"))?;
        reader.commit()
    }
}

impl PendingMessage {
    fn from_ref(message: MessageRef) -> Self {
        Self {
            seq: message.seq,
            timestamp_ns: message.timestamp_ns,
            type_id: message.type_id,
            payload_offset: message.payload_offset,
            payload_len: message.payload_len,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OwnedMergedMessage {
    pub source: usize,
    pub seq: u64,
    pub timestamp_ns: u64,
    pub type_id: u16,
    pub payload: Vec<u8>,
}

pub struct OwnedFanInReader {
    readers: Vec<Box<dyn OwnedStreamReader>>,
    pending: Vec<Option<StreamMessageOwned>>,
}

impl OwnedFanInReader {
    pub fn new(readers: Vec<Box<dyn OwnedStreamReader>>) -> Self {
        let pending = readers.iter().map(|_| None).collect();
        Self { readers, pending }
    }

    pub fn next(&mut self) -> Result<Option<OwnedMergedMessage>> {
        for (index, reader) in self.readers.iter_mut().enumerate() {
            if self.pending[index].is_none() {
                if let Some(message) = reader.next_owned()? {
                    self.pending[index] = Some(message);
                }
            }
        }

        let mut best: Option<(usize, u64)> = None;
        for (index, pending) in self.pending.iter().enumerate() {
            let Some(message) = pending.as_ref() else {
                continue;
            };
            let timestamp = message.timestamp_ns;
            match best {
                None => best = Some((index, timestamp)),
                Some((best_index, best_timestamp)) => {
                    if timestamp < best_timestamp
                        || (timestamp == best_timestamp && index < best_index)
                    {
                        best = Some((index, timestamp));
                    }
                }
            }
        }

        let Some((source, _)) = best else {
            return Ok(None);
        };

        let message = self.pending[source]
            .take()
            .ok_or(Error::Corrupt("pending message missing"))?;
        Ok(Some(OwnedMergedMessage {
            source,
            seq: message.seq,
            timestamp_ns: message.timestamp_ns,
            type_id: message.type_id,
            payload: message.payload,
        }))
    }

    pub fn commit(&mut self, source: usize) -> Result<()> {
        let reader = self
            .readers
            .get_mut(source)
            .ok_or(Error::Unsupported("invalid fan-in source"))?;
        reader.commit()
    }
}
