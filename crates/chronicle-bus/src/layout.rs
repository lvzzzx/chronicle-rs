use std::path::{Path, PathBuf};

pub struct BusLayout {
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StrategyId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategyEndpoints {
    pub orders_out: PathBuf,
    pub orders_in: PathBuf,
}

impl BusLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn strategy_endpoints(&self, id: &StrategyId) -> StrategyEndpoints {
        let base = self.root.join("orders").join("queue").join(&id.0);
        StrategyEndpoints {
            orders_out: base.join("orders_out"),
            orders_in: base.join("orders_in"),
        }
    }

    pub fn mark_ready(&self, endpoint_dir: &Path) -> std::io::Result<()> {
        crate::ready::mark_ready(endpoint_dir)
    }

    pub fn write_lease(&self, endpoint_dir: &Path, payload: &[u8]) -> std::io::Result<()> {
        crate::lease::write_lease(endpoint_dir, payload)
    }
}
