pub mod discovery;
pub mod lease;
pub mod ready;
pub mod registration;

pub use discovery::{
    DiscoveryEvent, RouterDiscovery, SubscriberConfig, SubscriberDiscovery, SubscriberEvent,
};
pub use chronicle_layout::{OrdersLayout, StrategyEndpoints, StrategyId};
pub use registration::ReaderRegistration;

pub fn mark_ready(endpoint_dir: &std::path::Path) -> std::io::Result<()> {
    ready::mark_ready(endpoint_dir)
}

pub fn write_lease(endpoint_dir: &std::path::Path, payload: &[u8]) -> std::io::Result<()> {
    lease::write_lease(endpoint_dir, payload)
}
