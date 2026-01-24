use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::core::{DisconnectReason, Queue, QueueReader, ReaderConfig, Result, StartMode};

#[derive(Clone, Debug)]
pub struct SubscriberConfig {
    pub poll_interval: Duration,
    pub writer_ttl: Duration,
    pub start_mode: StartMode,
}

impl Default for SubscriberConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(500),
            writer_ttl: Duration::from_secs(5),
            start_mode: StartMode::ResumeStrict,
        }
    }
}

pub enum SubscriberEvent {
    Connected(QueueReader),
    Disconnected(DisconnectReason),
    Waiting,
}

pub struct SubscriberDiscovery {
    path: PathBuf,
    reader: String,
    config: SubscriberConfig,
    last_poll: Option<Instant>,
}

impl SubscriberDiscovery {
    pub fn new(
        path: impl Into<PathBuf>,
        reader: impl Into<String>,
        config: SubscriberConfig,
    ) -> Self {
        Self {
            path: path.into(),
            reader: reader.into(),
            config,
            last_poll: None,
        }
    }

    pub fn poll(&mut self, current: Option<&QueueReader>) -> Result<Vec<SubscriberEvent>> {
        let mut events = Vec::new();

        let now = Instant::now();
        if !self.should_poll(now) {
            return Ok(events);
        }
        self.last_poll = Some(now);

        if let Some(reader) = current {
            if let Some(reason) = reader.detect_disconnect(self.config.writer_ttl)? {
                events.push(SubscriberEvent::Disconnected(reason));
            }
            return Ok(events);
        }

        let mut reader_config = ReaderConfig::default();
        reader_config.start_mode = self.config.start_mode;

        match Queue::try_open_subscriber_with_config(&self.path, &self.reader, reader_config)? {
            Some(reader) => events.push(SubscriberEvent::Connected(reader)),
            None => events.push(SubscriberEvent::Waiting),
        }

        Ok(events)
    }

    fn should_poll(&self, now: Instant) -> bool {
        if self.config.poll_interval.is_zero() {
            return true;
        }
        match self.last_poll {
            None => true,
            Some(last) => now.duration_since(last) >= self.config.poll_interval,
        }
    }
}
