use anyhow::Result;
use crate::replay::{BookEventPayload, ReplayEngine};

use super::feature::{FeatureSet, RowBuffer, RowWriter};
use super::sink::RowSink;

#[derive(Debug, Default, Clone, Copy)]
pub struct ExtractorStats {
    pub messages: u64,
    pub events: u64,
    pub rows_emitted: u64,
    pub batches_written: u64,
    pub skipped: u64,
}

pub struct Extractor<F, S> {
    engine: ReplayEngine,
    features: F,
    buffer: RowBuffer,
    sink: S,
    stats: ExtractorStats,
}

impl<F, S> Extractor<F, S>
where
    F: FeatureSet,
    S: RowSink,
{
    pub fn new(engine: ReplayEngine, features: F, buffer: RowBuffer, sink: S) -> Self {
        Self {
            engine,
            features,
            buffer,
            sink,
            stats: ExtractorStats::default(),
        }
    }

    pub fn engine_mut(&mut self) -> &mut ReplayEngine {
        &mut self.engine
    }

    pub fn stats(&self) -> ExtractorStats {
        self.stats
    }

    pub fn run(&mut self) -> Result<ExtractorStats> {
        let engine = &mut self.engine;
        let features = &mut self.features;
        let buffer = &mut self.buffer;
        let sink = &mut self.sink;
        let stats = &mut self.stats;

        while let Some(msg) = engine.next_message()? {
            stats.messages = stats.messages.saturating_add(1);

            let Some(event) = msg.book_event else {
                stats.skipped = stats.skipped.saturating_add(1);
                continue;
            };

            match event.payload {
                BookEventPayload::Snapshot { .. } | BookEventPayload::Diff { .. } => {
                    stats.events = stats.events.saturating_add(1);
                    let emit = features.should_emit(&event);
                    let mut writer = RowWriter::new(buffer, emit);
                    features.calculate(msg.book, &event, &mut writer);
                    if emit {
                        buffer.commit_row();
                        stats.rows_emitted = stats.rows_emitted.saturating_add(1);
                        if buffer.should_flush() {
                            let batch = buffer.flush()?;
                            sink.write_batch(batch)?;
                            stats.batches_written = stats.batches_written.saturating_add(1);
                        }
                    }
                }
                _ => {
                    stats.skipped = stats.skipped.saturating_add(1);
                }
            }
        }
        self.finish()?;
        Ok(self.stats)
    }

    fn flush_batch(&mut self) -> Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let batch = self.buffer.flush()?;
        self.sink.write_batch(batch)?;
        self.stats.batches_written = self.stats.batches_written.saturating_add(1);
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()> {
        self.flush_batch()?;
        self.sink.finish()
    }
}
