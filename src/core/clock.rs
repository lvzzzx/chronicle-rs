use std::time::{SystemTime, UNIX_EPOCH};

/// A source of timestamps for the queue.
///
/// This trait allows the user to choose between wall-clock time (slower, but standard)
/// and TSC-based time (faster, monotonic, but requires calibration).
pub trait Clock: Send + Sync + 'static {
    /// Returns the current timestamp in nanoseconds since the UNIX epoch.
    fn now(&self) -> u64;
}

/// A clock that uses `std::time::SystemTime`.
///
/// This is the default implementation. It is susceptible to NTP adjustments and
/// has higher latency (~20-50ns), but requires no calibration.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before UNIX epoch");
        u64::try_from(timestamp.as_nanos()).expect("system time exceeds timestamp range")
    }
}

/// A clock that uses the CPU's Time-Stamp Counter (TSC) via the `quanta` crate.
///
/// This is significantly faster (~6-10ns) and monotonic. It anchors to SystemTime
/// at initialization and then uses TSC ticks to progress, ensuring no backward jumps.
#[derive(Debug, Clone)]
pub struct QuantaClock {
    clock: quanta::Clock,
    start_wall_ns: u64,
    start_instant: quanta::Instant,
}

impl Default for QuantaClock {
    fn default() -> Self {
        let clock = quanta::Clock::new();
        let start_instant = clock.now();
        let start_wall_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before UNIX epoch")
            .as_nanos() as u64;

        Self {
            clock,
            start_wall_ns,
            start_instant,
        }
    }
}

impl QuantaClock {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Clock for QuantaClock {
    fn now(&self) -> u64 {
        let delta = self.clock.now().duration_since(self.start_instant);
        self.start_wall_ns + delta.as_nanos() as u64
    }
}
