//! Service discovery for strategies and routers.
//!
//! This module provides discovery mechanisms for dynamic strategy-router connections.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::bus::is_ready;

/// Unique identifier for a trading strategy.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StrategyId(pub String);

/// Discovered strategy connection endpoints.
#[derive(Debug, Clone)]
pub struct StrategyEndpoints {
    pub strategy_id: StrategyId,
    pub orders_out: PathBuf,
    pub orders_in: PathBuf,
}

/// Discovery event for strategies.
#[derive(Debug)]
pub enum DiscoveryEvent {
    /// New strategy became ready.
    Added {
        strategy: StrategyId,
        orders_out: PathBuf,
    },
    /// Strategy disconnected or removed.
    Removed { strategy: StrategyId },
}

/// Discovers strategies that are ready to receive connections from a router.
///
/// Scans the orders directory for strategies that have marked themselves as ready
/// by creating a `READY` marker file.
///
/// # Example
///
/// ```no_run
/// use chronicle::trading::discovery::{RouterDiscovery, DiscoveryEvent};
/// use std::path::Path;
///
/// let mut discovery = RouterDiscovery::new(Path::new("./bus"))?;
///
/// loop {
///     for event in discovery.poll()? {
///         match event {
///             DiscoveryEvent::Added { strategy, orders_out } => {
///                 println!("Strategy {} ready at {:?}", strategy.0, orders_out);
///             }
///             DiscoveryEvent::Removed { strategy } => {
///                 println!("Strategy {} disconnected", strategy.0);
///             }
///         }
///     }
///     std::thread::sleep(std::time::Duration::from_millis(100));
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct RouterDiscovery {
    root: PathBuf,
    known: HashSet<StrategyId>,
    poll_interval: Duration,
}

impl RouterDiscovery {
    /// Create a new discovery instance.
    ///
    /// # Arguments
    ///
    /// * `root` - Bus root directory
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        Ok(Self {
            root,
            known: HashSet::new(),
            poll_interval: Duration::from_millis(100),
        })
    }

    /// Set the polling interval (default: 100ms).
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Poll for discovery events (added/removed strategies).
    ///
    /// Returns a vector of events since the last poll.
    pub fn poll(&mut self) -> io::Result<Vec<DiscoveryEvent>> {
        let current = scan_ready_strategies(&self.root)?;
        let mut events = Vec::new();

        // Detect newly added strategies
        for strategy in &current {
            if !self.known.contains(strategy) {
                let orders_out = self.root
                    .join("orders")
                    .join("queue")
                    .join(&strategy.0)
                    .join("orders_out");

                events.push(DiscoveryEvent::Added {
                    strategy: strategy.clone(),
                    orders_out,
                });
            }
        }

        // Detect removed strategies
        for strategy in &self.known {
            if !current.contains(strategy) {
                events.push(DiscoveryEvent::Removed {
                    strategy: strategy.clone(),
                });
            }
        }

        self.known = current;
        Ok(events)
    }

    /// Get all currently known strategies.
    pub fn known_strategies(&self) -> &HashSet<StrategyId> {
        &self.known
    }

    /// Get the bus root path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get strategy endpoints for a given strategy ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the strategy_id contains invalid path components.
    pub fn strategy_endpoints(&self, strategy_id: &StrategyId) -> io::Result<StrategyEndpoints> {
        // Validate strategy_id
        if strategy_id.0.is_empty()
            || strategy_id.0 == "."
            || strategy_id.0 == ".."
            || strategy_id.0.contains('/')
            || strategy_id.0.contains('\\')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid strategy_id: {}", strategy_id.0),
            ));
        }

        let base = self.root.join("orders").join("queue").join(&strategy_id.0);
        Ok(StrategyEndpoints {
            strategy_id: strategy_id.clone(),
            orders_out: base.join("orders_out"),
            orders_in: base.join("orders_in"),
        })
    }
}

/// Scan for strategies that have marked themselves as ready.
fn scan_ready_strategies(root: &Path) -> io::Result<HashSet<StrategyId>> {
    let orders_queue = root.join("orders").join("queue");

    // If the directory doesn't exist yet, return empty set
    if !orders_queue.exists() {
        return Ok(HashSet::new());
    }

    let mut strategies = HashSet::new();

    for entry in std::fs::read_dir(&orders_queue)? {
        let entry = entry?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let strategy_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };

        // Check if orders_out has a READY marker
        let orders_out = path.join("orders_out");
        if is_ready(&orders_out) {
            strategies.insert(StrategyId(strategy_name));
        }
    }

    Ok(strategies)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::mark_ready;
    use crate::core::Queue;
    use tempfile::TempDir;

    #[test]
    fn test_discovery_empty() {
        let dir = TempDir::new().unwrap();
        let mut discovery = RouterDiscovery::new(dir.path()).unwrap();

        let events = discovery.poll().unwrap();
        assert!(events.is_empty());
        assert!(discovery.known_strategies().is_empty());
    }

    #[test]
    fn test_discovery_add_strategy() {
        let dir = TempDir::new().unwrap();
        let mut discovery = RouterDiscovery::new(dir.path()).unwrap();

        // Create a strategy and mark it ready
        let orders_out = dir
            .path()
            .join("orders")
            .join("queue")
            .join("alpha")
            .join("orders_out");
        Queue::open_publisher(&orders_out).unwrap();
        mark_ready(&orders_out).unwrap();

        // First poll should detect it
        let events = discovery.poll().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            DiscoveryEvent::Added { strategy, .. } => {
                assert_eq!(strategy.0, "alpha");
            }
            _ => panic!("expected Added event"),
        }

        // Second poll should be empty (no changes)
        let events = discovery.poll().unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_discovery_remove_strategy() {
        let dir = TempDir::new().unwrap();
        let mut discovery = RouterDiscovery::new(dir.path()).unwrap();

        // Create and mark ready
        let orders_out = dir
            .path()
            .join("orders")
            .join("queue")
            .join("alpha")
            .join("orders_out");
        Queue::open_publisher(&orders_out).unwrap();
        mark_ready(&orders_out).unwrap();

        // First poll: added
        discovery.poll().unwrap();

        // Remove the READY marker
        std::fs::remove_file(orders_out.join("READY")).unwrap();

        // Second poll should detect removal
        let events = discovery.poll().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            DiscoveryEvent::Removed { strategy } => {
                assert_eq!(strategy.0, "alpha");
            }
            _ => panic!("expected Removed event"),
        }
    }

    #[test]
    fn test_strategy_endpoints() {
        let dir = TempDir::new().unwrap();
        let discovery = RouterDiscovery::new(dir.path()).unwrap();

        let endpoints = discovery
            .strategy_endpoints(&StrategyId("alpha".to_string()))
            .unwrap();

        assert_eq!(endpoints.strategy_id.0, "alpha");
        assert_eq!(
            endpoints.orders_out,
            dir.path().join("orders/queue/alpha/orders_out")
        );
        assert_eq!(
            endpoints.orders_in,
            dir.path().join("orders/queue/alpha/orders_in")
        );
    }

    #[test]
    fn test_invalid_strategy_id() {
        let dir = TempDir::new().unwrap();
        let discovery = RouterDiscovery::new(dir.path()).unwrap();

        // Test path traversal attempts
        assert!(discovery
            .strategy_endpoints(&StrategyId("../evil".to_string()))
            .is_err());
        assert!(discovery
            .strategy_endpoints(&StrategyId("".to_string()))
            .is_err());
        assert!(discovery
            .strategy_endpoints(&StrategyId(".".to_string()))
            .is_err());
    }
}
