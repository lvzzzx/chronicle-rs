//! Path conventions for trading system components.
//!
//! This module defines the standard directory layout for a trading system built on Chronicle queues:
//!
//! ```text
//! {root}/
//! ├── streams/
//! │   ├── raw/{venue}/queue                    ← Raw market data feeds
//! │   └── clean/{venue}/{symbol}/{stream}/queue ← Processed streams
//! └── orders/
//!     └── queue/{strategy_id}/
//!         ├── orders_out/  ← Strategy → Router
//!         └── orders_in/   ← Router → Strategy
//! ```

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    EmptyComponent { field: &'static str },
    InvalidComponent { field: &'static str, value: String },
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::EmptyComponent { field } => {
                write!(f, "empty path component: {field}")
            }
            PathError::InvalidComponent { field, value } => {
                write!(f, "invalid path component for {field}: {value}")
            }
        }
    }
}

impl std::error::Error for PathError {}

type Result<T> = std::result::Result<T, PathError>;

/// Validates that a path component is safe (no path traversal).
///
/// # Errors
///
/// Returns `PathError` if:
/// - Component is empty
/// - Contains path separators (`/` or `\`)
/// - Is a special directory (`.` or `..`)
/// - Contains null bytes
pub fn validate_component(field: &'static str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(PathError::EmptyComponent { field });
    }
    if value == "." || value == ".." || value.contains('/') || value.contains('\\') {
        return Err(PathError::InvalidComponent {
            field,
            value: value.to_string(),
        });
    }
    if value.contains('\0') {
        return Err(PathError::InvalidComponent {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

/// Returns the path to a strategy's outbound orders queue.
///
/// Path: `{root}/orders/queue/{strategy_id}/orders_out`
pub fn strategy_orders_out_path(root: &Path, strategy_id: &str) -> Result<PathBuf> {
    validate_component("strategy_id", strategy_id)?;
    Ok(root
        .join("orders")
        .join("queue")
        .join(strategy_id)
        .join("orders_out"))
}

/// Returns the path to a strategy's inbound fills/acks queue.
///
/// Path: `{root}/orders/queue/{strategy_id}/orders_in`
pub fn strategy_orders_in_path(root: &Path, strategy_id: &str) -> Result<PathBuf> {
    validate_component("strategy_id", strategy_id)?;
    Ok(root
        .join("orders")
        .join("queue")
        .join(strategy_id)
        .join("orders_in"))
}

/// Returns the base path for all order queues.
///
/// Path: `{root}/orders/queue`
pub fn orders_root_path(root: &Path) -> PathBuf {
    root.join("orders").join("queue")
}

/// Returns the path to a raw market data feed queue.
///
/// Path: `{root}/streams/raw/{venue}/queue`
pub fn raw_feed_path(root: &Path, venue: &str) -> Result<PathBuf> {
    validate_component("venue", venue)?;
    Ok(root.join("streams").join("raw").join(venue).join("queue"))
}

/// Returns the path to a clean (processed) market data stream.
///
/// Path: `{root}/streams/clean/{venue}/{symbol}/{stream}/queue`
pub fn clean_stream_path(
    root: &Path,
    venue: &str,
    symbol: &str,
    stream: &str,
) -> Result<PathBuf> {
    validate_component("venue", venue)?;
    validate_component("symbol", symbol)?;
    validate_component("stream", stream)?;
    Ok(root
        .join("streams")
        .join("clean")
        .join(venue)
        .join(symbol)
        .join(stream)
        .join("queue"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_component_valid() {
        assert!(validate_component("test", "strategy_a").is_ok());
        assert!(validate_component("test", "binance").is_ok());
        assert!(validate_component("test", "btc-usdt").is_ok());
    }

    #[test]
    fn test_validate_component_empty() {
        let err = validate_component("test", "").unwrap_err();
        assert!(matches!(err, PathError::EmptyComponent { field: "test" }));
    }

    #[test]
    fn test_validate_component_path_traversal() {
        assert!(validate_component("test", "..").is_err());
        assert!(validate_component("test", ".").is_err());
        assert!(validate_component("test", "foo/bar").is_err());
        assert!(validate_component("test", "foo\\bar").is_err());
    }

    #[test]
    fn test_strategy_orders_out_path() {
        let root = Path::new("/tmp/bus");
        let path = strategy_orders_out_path(root, "strategy_a").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/bus/orders/queue/strategy_a/orders_out"));
    }

    #[test]
    fn test_strategy_orders_in_path() {
        let root = Path::new("/tmp/bus");
        let path = strategy_orders_in_path(root, "strategy_a").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/bus/orders/queue/strategy_a/orders_in"));
    }

    #[test]
    fn test_raw_feed_path() {
        let root = Path::new("/tmp/bus");
        let path = raw_feed_path(root, "binance").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/bus/streams/raw/binance/queue"));
    }

    #[test]
    fn test_clean_stream_path() {
        let root = Path::new("/tmp/bus");
        let path = clean_stream_path(root, "binance", "btc-usdt", "trades").unwrap();
        assert_eq!(
            path,
            PathBuf::from("/tmp/bus/streams/clean/binance/btc-usdt/trades/queue")
        );
    }

    #[test]
    fn test_orders_root_path() {
        let root = Path::new("/tmp/bus");
        let path = orders_root_path(root);
        assert_eq!(path, PathBuf::from("/tmp/bus/orders/queue"));
    }

    #[test]
    fn test_reject_invalid_strategy_id() {
        let root = Path::new("/tmp/bus");
        assert!(strategy_orders_out_path(root, "").is_err());
        assert!(strategy_orders_out_path(root, "../etc").is_err());
        assert!(strategy_orders_out_path(root, "foo/bar").is_err());
    }
}
