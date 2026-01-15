use crate::header::MessageHeader;
use crate::reader::{QueueMessage, QueueReader};
use crate::{Error, Result};

pub struct MergedMessage {
    pub source: usize,
    pub header: MessageHeader,
    pub payload: Vec<u8>,
}

pub struct FanInReader {
    readers: Vec<QueueReader>,
    pending: Vec<Option<QueueMessage>>,
}

impl FanInReader {
    pub fn new(readers: Vec<QueueReader>) -> Self {
        let pending = readers.iter().map(|_| None).collect();
        Self { readers, pending }
    }

    pub fn next(&mut self) -> Result<Option<MergedMessage>> {
        for (index, reader) in self.readers.iter_mut().enumerate() {
            if self.pending[index].is_none() {
                self.pending[index] = reader.next_message()?;
            }
        }

        let mut best: Option<(usize, u64)> = None;
        for (index, pending) in self.pending.iter().enumerate() {
            let Some(message) = pending.as_ref() else {
                continue;
            };
            let timestamp = message.header.timestamp_ns;
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
        Ok(Some(MergedMessage {
            source,
            header: message.header,
            payload: message.payload,
        }))
    }

    pub fn commit(&self, source: usize) -> Result<()> {
        let reader = self
            .readers
            .get(source)
            .ok_or(Error::Unsupported("invalid fan-in source"))?;
        reader.commit()
    }
}
