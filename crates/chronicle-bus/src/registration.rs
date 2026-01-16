use std::path::{Path, PathBuf};

pub struct ReaderRegistration {
    reader_name: String,
    meta_path: PathBuf,
}

impl ReaderRegistration {
    pub fn register(queue_dir: &Path, reader_name: &str) -> std::io::Result<Self> {
        if reader_name.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "reader name cannot be empty",
            ));
        }
        let readers_dir = queue_dir.join("readers");
        std::fs::create_dir_all(&readers_dir)?;
        let meta_path = readers_dir.join(format!("{reader_name}.meta"));
        Ok(Self {
            reader_name: reader_name.to_string(),
            meta_path,
        })
    }

    pub fn meta_path(&self) -> &Path {
        &self.meta_path
    }

    pub fn reader_name(&self) -> &str {
        &self.reader_name
    }
}

impl Drop for ReaderRegistration {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.meta_path);
    }
}
