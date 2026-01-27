use crate::stream::StreamReader;
use super::{Transform, Sink, TimeRangeFilter, TypeFilter, Sampler, TransformChain};
use anyhow::Result;
use std::time::{Duration, Instant};

/// Statistics for a replay run.
#[derive(Debug, Clone, Default)]
pub struct ReplayStats {
    pub messages_read: u64,
    pub messages_written: u64,
    pub messages_filtered: u64,
    pub duration: Duration,
}

impl ReplayStats {
    pub fn throughput(&self) -> f64 {
        if self.duration.as_secs_f64() > 0.0 {
            self.messages_read as f64 / self.duration.as_secs_f64()
        } else {
            0.0
        }
    }
}

/// Fluent API builder for replay operations.
pub struct ReplayPipeline<R: StreamReader> {
    source: R,
    transforms: TransformChain,
    sink: Option<Box<dyn Sink>>,
}

impl<R: StreamReader> ReplayPipeline<R> {
    /// Create a pipeline from a source reader.
    pub fn from_source(source: R) -> Self {
        Self {
            source,
            transforms: TransformChain::new(),
            sink: None,
        }
    }

    /// Filter messages by time range.
    pub fn filter_time_range(mut self, start_ns: u64, end_ns: u64) -> Self {
        self.transforms = self.transforms.add(TimeRangeFilter::new(start_ns, end_ns));
        self
    }

    /// Filter messages by type ID.
    pub fn filter_types(mut self, types: &[u16]) -> Self {
        self.transforms = self.transforms.add(TypeFilter::new(types));
        self
    }

    /// Sample every Nth message.
    pub fn sample(mut self, interval: usize) -> Self {
        self.transforms = self.transforms.add(Sampler::new(interval));
        self
    }

    /// Add a custom transform.
    pub fn transform<T: Transform + 'static>(mut self, transform: T) -> Self {
        self.transforms = self.transforms.add(transform);
        self
    }

    /// Set the output sink.
    pub fn sink<S: Sink + 'static>(mut self, sink: S) -> Self {
        self.sink = Some(Box::new(sink));
        self
    }

    /// Execute the pipeline.
    pub fn run(mut self) -> Result<ReplayStats> {
        let start = Instant::now();
        let mut stats = ReplayStats::default();

        while let Some(msg) = self.source.next()? {
            stats.messages_read += 1;

            // Apply transforms
            match self.transforms.transform(msg)? {
                Some(transformed) => {
                    // Write to sink if present
                    if let Some(sink) = &mut self.sink {
                        sink.write(&transformed)?;
                        stats.messages_written += 1;
                    }
                }
                None => {
                    stats.messages_filtered += 1;
                }
            }

            // Progress logging every 100k messages
            if stats.messages_read % 100_000 == 0 {
                let elapsed = start.elapsed().as_secs_f64();
                log::info!(
                    "Processed {} msgs ({:.0} msg/sec)",
                    stats.messages_read,
                    stats.messages_read as f64 / elapsed
                );
            }
        }

        // Flush sink
        if let Some(sink) = &mut self.sink {
            sink.flush()?;
        }

        stats.duration = start.elapsed();
        Ok(stats)
    }

    /// Seek to sequence number before running.
    pub fn seek_seq(mut self, seq: u64) -> Result<Self> {
        self.source.seek_seq(seq)?;
        Ok(self)
    }

    /// Seek to timestamp before running.
    pub fn seek_timestamp(mut self, timestamp_ns: u64) -> Result<Self> {
        self.source.seek_timestamp(timestamp_ns)?;
        Ok(self)
    }
}
