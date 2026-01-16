use std::path::PathBuf;

use crate::layout::{BusLayout, StrategyId};

pub struct RouterDiscovery {
    layout: BusLayout,
    initialized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryEvent {
    Added {
        strategy: StrategyId,
        orders_out: PathBuf,
    },
    Removed {
        strategy: StrategyId,
    },
}

impl RouterDiscovery {
    pub fn new(layout: BusLayout) -> std::io::Result<Self> {
        Ok(Self {
            layout,
            initialized: false,
        })
    }

    pub fn poll(&mut self) -> std::io::Result<Vec<DiscoveryEvent>> {
        if !self.initialized {
            self.initialized = true;
        }
        Ok(Vec::new())
    }

    pub fn layout(&self) -> &BusLayout {
        &self.layout
    }
}
