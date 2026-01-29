//! Partition roller implementations.
//!
//! Provides strategies for determining partition values from message timestamp.

use crate::table::{PartitionScheme, PartitionValues};
use crate::core::{Error, Result};

/// Timezone for date/time partitioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timezone {
    UTC,
    AsiaShanghai, // UTC+8
    AsiaTokyo,    // UTC+9
    AmericaNewYork, // UTC-5/-4 (with DST - simplified to -5 for now)
    Custom(i32),  // Custom offset in seconds
}

impl Timezone {
    /// Parse timezone from string.
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "UTC" => Ok(Timezone::UTC),
            "Asia/Shanghai" => Ok(Timezone::AsiaShanghai),
            "Asia/Tokyo" => Ok(Timezone::AsiaTokyo),
            "America/New_York" => Ok(Timezone::AmericaNewYork),
            _ => {
                // Try parsing as custom offset (e.g., "+0800" or "-0500")
                if s.starts_with('+') || s.starts_with('-') {
                    let hours: i32 = s[1..3].parse().map_err(|_| {
                        Error::InvalidPartition(format!("Invalid timezone offset: {}", s))
                    })?;
                    let minutes: i32 = s[3..5].parse().map_err(|_| {
                        Error::InvalidPartition(format!("Invalid timezone offset: {}", s))
                    })?;
                    let sign = if s.starts_with('-') { -1 } else { 1 };
                    let offset_seconds = sign * (hours * 3600 + minutes * 60);
                    Ok(Timezone::Custom(offset_seconds))
                } else {
                    Err(Error::InvalidPartition(format!(
                        "Unknown timezone: {}",
                        s
                    )))
                }
            }
        }
    }

    /// Get offset in seconds from UTC.
    pub fn offset_seconds(&self) -> i32 {
        match self {
            Timezone::UTC => 0,
            Timezone::AsiaShanghai => 8 * 3600,
            Timezone::AsiaTokyo => 9 * 3600,
            Timezone::AmericaNewYork => -5 * 3600,
            Timezone::Custom(offset) => *offset,
        }
    }

    /// Apply timezone offset to timestamp (nanoseconds).
    pub fn apply_offset(&self, timestamp_ns: u64) -> u64 {
        let offset_ns = self.offset_seconds() as i64 * 1_000_000_000;
        timestamp_ns.wrapping_add(offset_ns as u64)
    }
}

/// Strategy for determining partition from message.
pub trait PartitionRoller: Send {
    /// Determine partition values for a message.
    ///
    /// # Arguments
    ///
    /// * `timestamp_ns` - Message timestamp in nanoseconds since Unix epoch
    /// * `payload` - Message payload (can be used for custom partitioning logic)
    fn partition_for(&self, timestamp_ns: u64, payload: &[u8]) -> Result<PartitionValues>;
}

/// Partition by date (YYYY-MM-DD).
///
/// Extracts date from timestamp and uses static values for other partition keys.
pub struct DateRoller {
    date_key: String,
    timezone: Timezone,
    static_values: PartitionValues,
}

impl DateRoller {
    /// Create a new date roller.
    ///
    /// # Arguments
    ///
    /// * `scheme` - Partition scheme (must include the date_key)
    /// * `date_key` - Name of the date partition key
    /// * `timezone` - Timezone for date calculation
    /// * `static_values` - Static partition values (e.g., channel)
    pub fn new(
        _scheme: PartitionScheme,
        date_key: impl Into<String>,
        timezone: Timezone,
        static_values: PartitionValues,
    ) -> Self {
        Self {
            date_key: date_key.into(),
            timezone,
            static_values,
        }
    }
}

impl PartitionRoller for DateRoller {
    fn partition_for(&self, timestamp_ns: u64, _payload: &[u8]) -> Result<PartitionValues> {
        // Apply timezone offset
        let local_ts_ns = self.timezone.apply_offset(timestamp_ns);

        // Convert to seconds
        let ts_secs = local_ts_ns / 1_000_000_000;

        // Calculate date components
        let days_since_epoch = ts_secs / 86400;
        let (year, month, day) = days_since_epoch_to_ymd(days_since_epoch as i64);

        // Format as YYYY-MM-DD
        let date_str = format!("{:04}-{:02}-{:02}", year, month, day);

        // Merge with static values
        let mut values = self.static_values.clone();
        values.insert(&self.date_key, date_str);

        Ok(values)
    }
}

/// Partition by hour (YYYY-MM-DD-HH).
pub struct HourRoller {
    hour_key: String,
    timezone: Timezone,
    static_values: PartitionValues,
}

impl HourRoller {
    /// Create a new hour roller.
    pub fn new(
        _scheme: PartitionScheme,
        hour_key: impl Into<String>,
        timezone: Timezone,
        static_values: PartitionValues,
    ) -> Self {
        Self {
            hour_key: hour_key.into(),
            timezone,
            static_values,
        }
    }
}

impl PartitionRoller for HourRoller {
    fn partition_for(&self, timestamp_ns: u64, _payload: &[u8]) -> Result<PartitionValues> {
        let local_ts_ns = self.timezone.apply_offset(timestamp_ns);
        let ts_secs = local_ts_ns / 1_000_000_000;

        let days_since_epoch = ts_secs / 86400;
        let seconds_in_day = ts_secs % 86400;
        let hour = seconds_in_day / 3600;

        let (year, month, day) = days_since_epoch_to_ymd(days_since_epoch as i64);

        let hour_str = format!("{:04}-{:02}-{:02}-{:02}", year, month, day, hour);

        let mut values = self.static_values.clone();
        values.insert(&self.hour_key, hour_str);

        Ok(values)
    }
}

/// Partition by minute (YYYY-MM-DD-HH-mm).
pub struct MinuteRoller {
    minute_key: String,
    timezone: Timezone,
    static_values: PartitionValues,
}

impl MinuteRoller {
    /// Create a new minute roller.
    pub fn new(
        _scheme: PartitionScheme,
        minute_key: impl Into<String>,
        timezone: Timezone,
        static_values: PartitionValues,
    ) -> Self {
        Self {
            minute_key: minute_key.into(),
            timezone,
            static_values,
        }
    }
}

impl PartitionRoller for MinuteRoller {
    fn partition_for(&self, timestamp_ns: u64, _payload: &[u8]) -> Result<PartitionValues> {
        let local_ts_ns = self.timezone.apply_offset(timestamp_ns);
        let ts_secs = local_ts_ns / 1_000_000_000;

        let days_since_epoch = ts_secs / 86400;
        let seconds_in_day = ts_secs % 86400;
        let hour = seconds_in_day / 3600;
        let minute = (seconds_in_day % 3600) / 60;

        let (year, month, day) = days_since_epoch_to_ymd(days_since_epoch as i64);

        let minute_str = format!("{:04}-{:02}-{:02}-{:02}-{:02}", year, month, day, hour, minute);

        let mut values = self.static_values.clone();
        values.insert(&self.minute_key, minute_str);

        Ok(values)
    }
}

/// Convert days since Unix epoch to (year, month, day).
///
/// Uses a simplified algorithm suitable for years 1970-2100.
fn days_since_epoch_to_ymd(mut days: i64) -> (i32, u8, u8) {
    // Days since 1970-01-01
    let mut year = 1970;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    // Find month and day
    let days_in_months = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1;
    for &days_in_month in &days_in_months {
        if days < days_in_month as i64 {
            break;
        }
        days -= days_in_month as i64;
        month += 1;
    }

    let day = (days + 1) as u8; // +1 because days are 0-indexed

    (year, month, day)
}

/// Check if a year is a leap year.
fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::PartitionValue;

    #[test]
    fn test_timezone_offset() {
        assert_eq!(Timezone::UTC.offset_seconds(), 0);
        assert_eq!(Timezone::AsiaShanghai.offset_seconds(), 8 * 3600);
        assert_eq!(Timezone::AsiaTokyo.offset_seconds(), 9 * 3600);
        assert_eq!(Timezone::AmericaNewYork.offset_seconds(), -5 * 3600);
    }

    #[test]
    fn test_timezone_from_str() {
        assert_eq!(Timezone::from_str("UTC").unwrap(), Timezone::UTC);
        assert_eq!(
            Timezone::from_str("Asia/Shanghai").unwrap(),
            Timezone::AsiaShanghai
        );
        assert_eq!(
            Timezone::from_str("+0800").unwrap().offset_seconds(),
            8 * 3600
        );
        assert_eq!(
            Timezone::from_str("-0500").unwrap().offset_seconds(),
            -5 * 3600
        );
    }

    #[test]
    fn test_days_since_epoch_to_ymd() {
        // 1970-01-01
        assert_eq!(days_since_epoch_to_ymd(0), (1970, 1, 1));

        // 2024-01-29 (19751 days since epoch)
        assert_eq!(days_since_epoch_to_ymd(19751), (2024, 1, 29));

        // 2000-01-01 (leap year)
        assert_eq!(days_since_epoch_to_ymd(10957), (2000, 1, 1));

        // 2000-02-29 (leap day)
        assert_eq!(days_since_epoch_to_ymd(11016), (2000, 2, 29));
    }

    #[test]
    fn test_date_roller() {
        let scheme = PartitionScheme::new()
            .add_string("channel")
            .add_date("date");

        let roller = DateRoller::new(
            scheme,
            "date",
            Timezone::UTC,
            [("channel", "101")].into(),
        );

        // 2024-01-29 00:00:00 UTC = 1706486400 seconds
        let ts_ns = 1_706_486_400_000_000_000u64;

        let values = roller.partition_for(ts_ns, b"").unwrap();

        assert_eq!(
            values.get("date"),
            Some(&PartitionValue::String("2024-01-29".to_string()))
        );
        assert_eq!(
            values.get("channel"),
            Some(&PartitionValue::String("101".to_string()))
        );
    }

    #[test]
    fn test_date_roller_timezone() {
        let scheme = PartitionScheme::new()
            .add_string("channel")
            .add_date("date");

        let roller = DateRoller::new(
            scheme,
            "date",
            Timezone::AsiaShanghai, // UTC+8
            [("channel", "101")].into(),
        );

        // 2024-01-29 00:00:00 UTC
        // = 2024-01-29 08:00:00 Asia/Shanghai (same day)
        let ts_ns = 1_706_486_400_000_000_000u64;

        let values = roller.partition_for(ts_ns, b"").unwrap();

        assert_eq!(
            values.get("date"),
            Some(&PartitionValue::String("2024-01-29".to_string()))
        );

        // 2024-01-28 23:00:00 UTC
        // = 2024-01-29 07:00:00 Asia/Shanghai (next day!)
        let ts_ns_minus_1h = ts_ns - 3600_000_000_000;

        let values2 = roller.partition_for(ts_ns_minus_1h, b"").unwrap();

        assert_eq!(
            values2.get("date"),
            Some(&PartitionValue::String("2024-01-29".to_string())) // Still Jan 29 in Shanghai
        );
    }

    #[test]
    fn test_hour_roller() {
        let scheme = PartitionScheme::new().add_hour("hour");

        let roller = HourRoller::new(scheme, "hour", Timezone::UTC, PartitionValues::new());

        // 2024-01-29 15:30:00 UTC
        let ts_ns = 1_706_486_400_000_000_000u64 + (15 * 3600 + 30 * 60) * 1_000_000_000;

        let values = roller.partition_for(ts_ns, b"").unwrap();

        assert_eq!(
            values.get("hour"),
            Some(&PartitionValue::String("2024-01-29-15".to_string()))
        );
    }

    #[test]
    fn test_minute_roller() {
        let scheme = PartitionScheme::new().add_minute("minute");

        let roller = MinuteRoller::new(scheme, "minute", Timezone::UTC, PartitionValues::new());

        // 2024-01-29 15:42:00 UTC
        let ts_ns = 1_706_486_400_000_000_000u64 + (15 * 3600 + 42 * 60) * 1_000_000_000;

        let values = roller.partition_for(ts_ns, b"").unwrap();

        assert_eq!(
            values.get("minute"),
            Some(&PartitionValue::String("2024-01-29-15-42".to_string()))
        );
    }
}
