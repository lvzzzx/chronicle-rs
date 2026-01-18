use std::path::PathBuf;
use std::time::{Duration, Instant};

use chronicle_core::{DisconnectReason, Queue, QueueReader, ReaderConfig, Result, StartMode};

#[derive(Clone, Debug)]
pub struct PassiveConfig {
    pub poll_interval: Duration,
    pub writer_ttl: Duration,
    pub start_mode: StartMode,
}

impl Default for PassiveConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(500),
            writer_ttl: Duration::from_secs(5),
            start_mode: StartMode::ResumeStrict,
        }
    }
}

pub enum PassiveEvent {
    Connected(QueueReader),
    Disconnected(DisconnectReason),
    Waiting,
}

pub struct PassiveDiscovery {
    path: PathBuf,
    reader: String,
    config: PassiveConfig,
    last_poll: Option<Instant>,
}

impl PassiveDiscovery {
    pub fn new(
        path: impl Into<PathBuf>,
        reader: impl Into<String>,
        config: PassiveConfig,
    ) -> Self {
        Self {
            path: path.into(),
            reader: reader.into(),
            config,
            last_poll: None,
        }
    }

    pub fn poll(&mut self, current: Option<&QueueReader>) -> Result<Vec<PassiveEvent>> {
        let mut events = Vec::new();
        if let Some(reader) = current {
            if let Some(reason) = reader.detect_disconnect(self.config.writer_ttl)? {
                events.push(PassiveEvent::Disconnected(reason));
            }
            return Ok(events);
        }

        let now = Instant::now();
        if !self.should_poll(now) {
            return Ok(events);
        }
        self.last_poll = Some(now);

        let mut reader_config = ReaderConfig::default();
        reader_config.start_mode = self.config.start_mode;

        match Queue::try_open_subscriber_with_config(&self.path, &self.reader, reader_config)? {
            Some(reader) => events.push(PassiveEvent::Connected(reader)),
            None => events.push(PassiveEvent::Waiting),
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
