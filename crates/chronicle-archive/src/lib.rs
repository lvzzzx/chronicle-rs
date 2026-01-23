use std::path::{Path, PathBuf};

use anyhow::Result;
use chronicle_core::{Queue, ReaderConfig, StartMode, WriterConfig};

pub struct ArchiveTap {
    live_path: PathBuf,
    archive_path: PathBuf,
    reader_name: String,
    start_mode: StartMode,
}

impl ArchiveTap {
    pub fn new(live_path: impl AsRef<Path>, archive_path: impl AsRef<Path>) -> Self {
        Self {
            live_path: live_path.as_ref().to_path_buf(),
            archive_path: archive_path.as_ref().to_path_buf(),
            reader_name: "archive_tap".to_string(),
            start_mode: StartMode::ResumeStrict,
        }
    }

    pub fn reader_name(mut self, reader_name: impl Into<String>) -> Self {
        self.reader_name = reader_name.into();
        self
    }

    pub fn start_mode(mut self, start_mode: StartMode) -> Self {
        self.start_mode = start_mode;
        self
    }

    pub fn run(&self) -> Result<()> {
        let config = ReaderConfig {
            memlock: false,
            start_mode: self.start_mode,
        };
        let mut reader = Queue::open_subscriber_with_config(
            &self.live_path,
            &self.reader_name,
            config,
        )?;
        let mut writer = Queue::open_publisher_with_config(
            &self.archive_path,
            WriterConfig::archive(),
        )?;

        loop {
            reader.wait(None)?;
            let Some(msg) = reader.next()? else {
                continue;
            };
            writer.append_with_timestamp(msg.type_id, msg.payload, msg.timestamp_ns)?;
            reader.commit()?;
        }
    }
}
