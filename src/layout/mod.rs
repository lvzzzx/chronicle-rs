use std::fmt;
use std::path::{Path, PathBuf};

pub const ARCHIVE_VERSION: &str = "v1";

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
        let path = layout
            .streams()
            .raw_queue_dir("binance")
            .expect("raw path");
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
        let err = layout
            .streams()
            .raw_queue_dir("bad/venue")
            .unwrap_err();
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
}
