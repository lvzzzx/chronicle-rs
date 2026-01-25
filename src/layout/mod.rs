use std::fmt;
use std::path::{Path, PathBuf};

pub const ARCHIVE_VERSION: &str = "v1";
const SEGMENT_WIDTH: usize = 9;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    EmptyComponent { field: &'static str },
    InvalidComponent { field: &'static str, value: String },
    InvalidDate { value: String },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayoutError::EmptyComponent { field } => {
                write!(f, "empty path component: {field}")
            }
            LayoutError::InvalidComponent { field, value } => {
                write!(f, "invalid path component for {field}: {value}")
            }
            LayoutError::InvalidDate { value } => {
                write!(f, "invalid date format (expected YYYY-MM-DD): {value}")
            }
        }
    }
}

impl std::error::Error for LayoutError {}

type Result<T> = std::result::Result<T, LayoutError>;

#[derive(Debug, Clone)]
pub struct IpcLayout {
    root: PathBuf,
}

impl IpcLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn streams(&self) -> StreamsLayout {
        StreamsLayout::new(&self.root)
    }

    pub fn orders(&self) -> OrdersLayout {
        OrdersLayout::new(&self.root)
    }
}

#[derive(Debug, Clone)]
pub struct StreamsLayout {
    root: PathBuf,
}

impl StreamsLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn raw_queue_dir(&self, venue: &str) -> Result<PathBuf> {
        validate_component("venue", venue)?;
        Ok(self
            .root
            .join("streams")
            .join("raw")
            .join(venue)
            .join("queue"))
    }

    pub fn raw_queue_segment_path(&self, venue: &str, segment_id: u64) -> Result<PathBuf> {
        Ok(self
            .raw_queue_dir(venue)?
            .join(segment_file_name(segment_id)))
    }

    pub fn clean_queue_dir(&self, venue: &str, symbol: &str, stream: &str) -> Result<PathBuf> {
        validate_component("venue", venue)?;
        validate_component("symbol", symbol)?;
        validate_component("stream", stream)?;
        Ok(self
            .root
            .join("streams")
            .join("clean")
            .join(venue)
            .join(symbol)
            .join(stream)
            .join("queue"))
    }

    pub fn clean_queue_segment_path(
        &self,
        venue: &str,
        symbol: &str,
        stream: &str,
        segment_id: u64,
    ) -> Result<PathBuf> {
        Ok(self
            .clean_queue_dir(venue, symbol, stream)?
            .join(segment_file_name(segment_id)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StrategyId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategyEndpoints {
    pub orders_out: PathBuf,
    pub orders_in: PathBuf,
}

#[derive(Debug, Clone)]
pub struct OrdersLayout {
    root: PathBuf,
}

impl OrdersLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn orders_root(&self) -> PathBuf {
        self.root.join("orders").join("queue")
    }

    pub fn strategy_endpoints(&self, id: &StrategyId) -> Result<StrategyEndpoints> {
        validate_component("strategy_id", &id.0)?;
        let base = self.orders_root().join(&id.0);
        Ok(StrategyEndpoints {
            orders_out: base.join("orders_out"),
            orders_in: base.join("orders_in"),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ArchiveLayout {
    root: PathBuf,
}

impl ArchiveLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn stream_dir(
        &self,
        venue: &str,
        symbol: &str,
        date: &str,
        stream: &str,
    ) -> Result<PathBuf> {
        validate_component("venue", venue)?;
        validate_component("symbol", symbol)?;
        validate_date(date)?;
        validate_component("stream", stream)?;
        Ok(self
            .root
            .join(ARCHIVE_VERSION)
            .join(venue)
            .join(symbol)
            .join(date)
            .join(stream))
    }

    pub fn stream_meta_path(
        &self,
        venue: &str,
        symbol: &str,
        date: &str,
        stream: &str,
    ) -> Result<PathBuf> {
        Ok(self
            .stream_dir(venue, symbol, date, stream)?
            .join("meta.json"))
    }

    pub fn stream_segment_path(
        &self,
        venue: &str,
        symbol: &str,
        date: &str,
        stream: &str,
        segment_id: u64,
    ) -> Result<PathBuf> {
        Ok(self
            .stream_dir(venue, symbol, date, stream)?
            .join(segment_file_name(segment_id)))
    }

    pub fn stream_compressed_segment_path(
        &self,
        venue: &str,
        symbol: &str,
        date: &str,
        stream: &str,
        segment_id: u64,
    ) -> Result<PathBuf> {
        Ok(self
            .stream_dir(venue, symbol, date, stream)?
            .join(compressed_segment_file_name(segment_id)))
    }

    pub fn stream_compressed_index_path(
        &self,
        venue: &str,
        symbol: &str,
        date: &str,
        stream: &str,
        segment_id: u64,
    ) -> Result<PathBuf> {
        Ok(self
            .stream_dir(venue, symbol, date, stream)?
            .join(compressed_index_file_name(segment_id)))
    }

    pub fn stream_remote_meta_path(
        &self,
        venue: &str,
        symbol: &str,
        date: &str,
        stream: &str,
        segment_id: u64,
    ) -> Result<PathBuf> {
        Ok(self
            .stream_dir(venue, symbol, date, stream)?
            .join(remote_meta_file_name(segment_id)))
    }
}

#[derive(Debug, Clone)]
pub struct RawArchiveLayout {
    root: PathBuf,
}

impl RawArchiveLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn raw_dir(&self, venue: &str, date: &str) -> Result<PathBuf> {
        validate_component("venue", venue)?;
        validate_date(date)?;
        Ok(self
            .root
            .join("raw")
            .join(ARCHIVE_VERSION)
            .join(venue)
            .join(date)
            .join("raw"))
    }

    pub fn raw_meta_path(&self, venue: &str, date: &str) -> Result<PathBuf> {
        Ok(self.raw_dir(venue, date)?.join("meta.json"))
    }

    pub fn raw_segment_path(&self, venue: &str, date: &str, segment_id: u64) -> Result<PathBuf> {
        Ok(self
            .raw_dir(venue, date)?
            .join(segment_file_name(segment_id)))
    }

    pub fn raw_compressed_segment_path(
        &self,
        venue: &str,
        date: &str,
        segment_id: u64,
    ) -> Result<PathBuf> {
        Ok(self
            .raw_dir(venue, date)?
            .join(compressed_segment_file_name(segment_id)))
    }

    pub fn raw_compressed_index_path(
        &self,
        venue: &str,
        date: &str,
        segment_id: u64,
    ) -> Result<PathBuf> {
        Ok(self
            .raw_dir(venue, date)?
            .join(compressed_index_file_name(segment_id)))
    }

    pub fn raw_remote_meta_path(
        &self,
        venue: &str,
        date: &str,
        segment_id: u64,
    ) -> Result<PathBuf> {
        Ok(self
            .raw_dir(venue, date)?
            .join(remote_meta_file_name(segment_id)))
    }
}

fn segment_file_name(segment_id: u64) -> String {
    format!("{:0width$}.q", segment_id, width = SEGMENT_WIDTH)
}

fn compressed_segment_file_name(segment_id: u64) -> String {
    format!("{:0width$}.q.zst", segment_id, width = SEGMENT_WIDTH)
}

fn compressed_index_file_name(segment_id: u64) -> String {
    format!("{:0width$}.q.zst.idx", segment_id, width = SEGMENT_WIDTH)
}

fn remote_meta_file_name(segment_id: u64) -> String {
    format!(
        "{:0width$}.q.zst.remote.json",
        segment_id,
        width = SEGMENT_WIDTH
    )
}

fn validate_component(field: &'static str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(LayoutError::EmptyComponent { field });
    }
    if value == "." || value == ".." || value.contains('/') || value.contains('\\') {
        return Err(LayoutError::InvalidComponent {
            field,
            value: value.to_string(),
        });
    }
    if value.contains('\0') {
        return Err(LayoutError::InvalidComponent {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_date(value: &str) -> Result<()> {
    if value.len() != 10 {
        return Err(LayoutError::InvalidDate {
            value: value.to_string(),
        });
    }
    let bytes = value.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(LayoutError::InvalidDate {
            value: value.to_string(),
        });
    }
    for (idx, byte) in bytes.iter().enumerate() {
        if idx == 4 || idx == 7 {
            continue;
        }
        if !byte.is_ascii_digit() {
            return Err(LayoutError::InvalidDate {
                value: value.to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_raw_queue_path() {
        let layout = IpcLayout::new("/tmp/bus");
        let path = layout.streams().raw_queue_dir("binance").expect("raw path");
        assert_eq!(path, PathBuf::from("/tmp/bus/streams/raw/binance/queue"));
    }

    #[test]
    fn archive_stream_path() {
        let layout = ArchiveLayout::new("/data/archive");
        let path = layout
            .stream_dir("binance", "btc-usdt", "2026-01-24", "trades")
            .expect("stream path");
        assert_eq!(
            path,
            PathBuf::from("/data/archive/v1/binance/btc-usdt/2026-01-24/trades")
        );
    }

    #[test]
    fn reject_invalid_component() {
        let layout = IpcLayout::new("/tmp/bus");
        let err = layout.streams().raw_queue_dir("bad/venue").unwrap_err();
        assert!(matches!(err, LayoutError::InvalidComponent { .. }));
    }

    #[test]
    fn reject_invalid_date() {
        let layout = ArchiveLayout::new("/data/archive");
        let err = layout
            .stream_dir("binance", "btc-usdt", "20260124", "trades")
            .unwrap_err();
        assert!(matches!(err, LayoutError::InvalidDate { .. }));
    }

    #[test]
    fn orders_endpoints_path() {
        let layout = IpcLayout::new("/tmp/bus").orders();
        let endpoints = layout
            .strategy_endpoints(&StrategyId("alpha".to_string()))
            .expect("orders endpoints");
        assert_eq!(
            endpoints.orders_out,
            PathBuf::from("/tmp/bus/orders/queue/alpha/orders_out")
        );
        assert_eq!(
            endpoints.orders_in,
            PathBuf::from("/tmp/bus/orders/queue/alpha/orders_in")
        );
    }

    #[test]
    fn stream_segment_path() {
        let layout = ArchiveLayout::new("/data/archive");
        let path = layout
            .stream_segment_path("binance", "btc-usdt", "2026-01-24", "trades", 42)
            .expect("segment path");
        assert_eq!(
            path,
            PathBuf::from("/data/archive/v1/binance/btc-usdt/2026-01-24/trades/000000042.q")
        );
    }

    #[test]
    fn raw_archive_segment_path() {
        let layout = RawArchiveLayout::new("/data/archive");
        let path = layout
            .raw_segment_path("binance", "2026-01-24", 42)
            .expect("raw segment path");
        assert_eq!(
            path,
            PathBuf::from("/data/archive/raw/v1/binance/2026-01-24/raw/000000042.q")
        );
    }
}
