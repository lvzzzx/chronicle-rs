use crate::core::Result;
use crate::stream::StreamMessageRef;
use std::collections::HashSet;

/// Transform trait for filtering and mapping messages.
pub trait Transform: Send {
    /// Transform a message. Returns None to filter out.
    fn transform<'a>(&mut self, msg: StreamMessageRef<'a>) -> Result<Option<StreamMessageRef<'a>>>;
}

/// No-op transform (pass-through).
pub struct Identity;

impl Transform for Identity {
    fn transform<'a>(&mut self, msg: StreamMessageRef<'a>) -> Result<Option<StreamMessageRef<'a>>> {
        Ok(Some(msg))
    }
}

/// Filter messages by time range.
pub struct TimeRangeFilter {
    start_ns: u64,
    end_ns: u64,
}

impl TimeRangeFilter {
    pub fn new(start_ns: u64, end_ns: u64) -> Self {
        Self { start_ns, end_ns }
    }
}

impl Transform for TimeRangeFilter {
    fn transform<'a>(&mut self, msg: StreamMessageRef<'a>) -> Result<Option<StreamMessageRef<'a>>> {
        if msg.timestamp_ns >= self.start_ns && msg.timestamp_ns < self.end_ns {
            Ok(Some(msg))
        } else {
            Ok(None)
        }
    }
}

/// Filter messages by type ID.
pub struct TypeFilter {
    types: HashSet<u16>,
}

impl TypeFilter {
    pub fn new(types: &[u16]) -> Self {
        Self {
            types: types.iter().copied().collect(),
        }
    }
}

impl Transform for TypeFilter {
    fn transform<'a>(&mut self, msg: StreamMessageRef<'a>) -> Result<Option<StreamMessageRef<'a>>> {
        if self.types.contains(&msg.type_id) {
            Ok(Some(msg))
        } else {
            Ok(None)
        }
    }
}

/// Sample every Nth message.
pub struct Sampler {
    interval: usize,
    count: usize,
}

impl Sampler {
    pub fn new(interval: usize) -> Self {
        Self { interval, count: 0 }
    }
}

impl Transform for Sampler {
    fn transform<'a>(&mut self, msg: StreamMessageRef<'a>) -> Result<Option<StreamMessageRef<'a>>> {
        self.count += 1;
        if self.count % self.interval == 0 {
            Ok(Some(msg))
        } else {
            Ok(None)
        }
    }
}

/// Chain multiple transforms.
pub struct TransformChain {
    transforms: Vec<Box<dyn Transform>>,
}

impl TransformChain {
    pub fn new() -> Self {
        Self {
            transforms: Vec::new(),
        }
    }

    pub fn add<T: Transform + 'static>(mut self, transform: T) -> Self {
        self.transforms.push(Box::new(transform));
        self
    }
}

impl Default for TransformChain {
    fn default() -> Self {
        Self::new()
    }
}

impl Transform for TransformChain {
    fn transform<'a>(&mut self, mut msg: StreamMessageRef<'a>) -> Result<Option<StreamMessageRef<'a>>> {
        for t in &mut self.transforms {
            match t.transform(msg)? {
                Some(m) => msg = m,
                None => return Ok(None),
            }
        }
        Ok(Some(msg))
    }
}
