//! Partition scheme and value types.
//!
//! Defines the structure of table partitions and how to represent partition values.

use crate::core::{Error, Result};
use std::collections::HashMap;
use std::fmt;

/// Partition key type definition.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum PartitionKey {
    /// String-valued partition key (e.g., symbol, channel).
    String {
        name: String,
    },
    /// Integer-valued partition key (e.g., shard_id).
    Int {
        name: String,
    },
    /// Date-valued partition key (YYYY-MM-DD format).
    Date {
        name: String,
        #[serde(default)]
        format: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timezone: Option<String>,
    },
    /// Hour-valued partition key (YYYY-MM-DD-HH format).
    Hour {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timezone: Option<String>,
    },
    /// Minute-valued partition key (YYYY-MM-DD-HH-mm format).
    Minute {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timezone: Option<String>,
    },
}

impl PartitionKey {
    /// Get the key name.
    pub fn name(&self) -> &str {
        match self {
            PartitionKey::String { name } => name,
            PartitionKey::Int { name } => name,
            PartitionKey::Date { name, .. } => name,
            PartitionKey::Hour { name, .. } => name,
            PartitionKey::Minute { name, .. } => name,
        }
    }

    /// Validate a value for this key type.
    pub fn validate_value(&self, value: &PartitionValue) -> Result<()> {
        match (self, value) {
            (PartitionKey::String { .. }, PartitionValue::String(_)) => Ok(()),
            (PartitionKey::Int { .. }, PartitionValue::Int(_)) => Ok(()),
            (PartitionKey::Date { .. }, PartitionValue::String(_)) => Ok(()),
            (PartitionKey::Hour { .. }, PartitionValue::String(_)) => Ok(()),
            (PartitionKey::Minute { .. }, PartitionValue::String(_)) => Ok(()),
            _ => Err(Error::InvalidPartition(format!(
                "Type mismatch for key '{}': expected {:?}, got {:?}",
                self.name(),
                self,
                value
            ))),
        }
    }
}

/// Partition scheme defining the structure of partitions.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PartitionScheme {
    pub(crate) keys: Vec<PartitionKey>,
}

impl PartitionScheme {
    /// Create an empty partition scheme.
    pub fn new() -> Self {
        Self { keys: Vec::new() }
    }

    /// Add a string partition key.
    pub fn add_string(mut self, name: &str) -> Self {
        self.keys.push(PartitionKey::String {
            name: name.to_string(),
        });
        self
    }

    /// Add an integer partition key.
    pub fn add_int(mut self, name: &str) -> Self {
        self.keys.push(PartitionKey::Int {
            name: name.to_string(),
        });
        self
    }

    /// Add a date partition key (YYYY-MM-DD).
    pub fn add_date(mut self, name: &str) -> Self {
        self.keys.push(PartitionKey::Date {
            name: name.to_string(),
            format: "YYYY-MM-DD".to_string(),
            timezone: None,
        });
        self
    }

    /// Add a date partition key with timezone.
    pub fn add_date_with_tz(mut self, name: &str, timezone: &str) -> Self {
        self.keys.push(PartitionKey::Date {
            name: name.to_string(),
            format: "YYYY-MM-DD".to_string(),
            timezone: Some(timezone.to_string()),
        });
        self
    }

    /// Add an hour partition key (YYYY-MM-DD-HH).
    pub fn add_hour(mut self, name: &str) -> Self {
        self.keys.push(PartitionKey::Hour {
            name: name.to_string(),
            timezone: None,
        });
        self
    }

    /// Add an hour partition key with timezone.
    pub fn add_hour_with_tz(mut self, name: &str, timezone: &str) -> Self {
        self.keys.push(PartitionKey::Hour {
            name: name.to_string(),
            timezone: Some(timezone.to_string()),
        });
        self
    }

    /// Add a minute partition key (YYYY-MM-DD-HH-mm).
    pub fn add_minute(mut self, name: &str) -> Self {
        self.keys.push(PartitionKey::Minute {
            name: name.to_string(),
            timezone: None,
        });
        self
    }

    /// Add a minute partition key with timezone.
    pub fn add_minute_with_tz(mut self, name: &str, timezone: &str) -> Self {
        self.keys.push(PartitionKey::Minute {
            name: name.to_string(),
            timezone: Some(timezone.to_string()),
        });
        self
    }

    /// Get partition keys.
    pub fn keys(&self) -> &[PartitionKey] {
        &self.keys
    }

    /// Validate partition values against this scheme.
    pub fn validate(&self, values: &PartitionValues) -> Result<()> {
        // Check all required keys are present
        for key in &self.keys {
            let value = values
                .0
                .get(key.name())
                .ok_or_else(|| Error::InvalidPartition(format!("Missing key '{}'", key.name())))?;
            key.validate_value(value)?;
        }

        // Check no extra keys
        for name in values.0.keys() {
            if !self.keys.iter().any(|k| k.name() == name) {
                return Err(Error::InvalidPartition(format!(
                    "Unknown partition key '{}'",
                    name
                )));
            }
        }

        Ok(())
    }

    /// Check if this scheme is empty (no partition keys).
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Number of partition keys.
    pub fn len(&self) -> usize {
        self.keys.len()
    }
}

impl Default for PartitionScheme {
    fn default() -> Self {
        Self::new()
    }
}

/// A single partition value (string or integer).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum PartitionValue {
    String(String),
    Int(i64),
}

impl fmt::Display for PartitionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PartitionValue::String(s) => write!(f, "{}", s),
            PartitionValue::Int(i) => write!(f, "{}", i),
        }
    }
}

impl From<&str> for PartitionValue {
    fn from(s: &str) -> Self {
        PartitionValue::String(s.to_string())
    }
}

impl From<String> for PartitionValue {
    fn from(s: String) -> Self {
        PartitionValue::String(s)
    }
}

impl From<i64> for PartitionValue {
    fn from(i: i64) -> Self {
        PartitionValue::Int(i)
    }
}

/// Partition values (key-value map).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PartitionValues(pub(crate) HashMap<String, PartitionValue>);

impl PartitionValues {
    /// Create empty partition values.
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    /// Insert a value.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<PartitionValue>) {
        self.0.insert(key.into(), value.into());
    }

    /// Get a value.
    pub fn get(&self, key: &str) -> Option<&PartitionValue> {
        self.0.get(key)
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of key-value pairs.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Iterate over key-value pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &PartitionValue)> {
        self.0.iter()
    }

    /// Convert to Hive-style path (e.g., "channel=101/date=2026-01-29").
    pub fn to_path(&self, scheme: &PartitionScheme) -> String {
        let mut parts = Vec::new();
        for key in &scheme.keys {
            if let Some(value) = self.0.get(key.name()) {
                parts.push(format!("{}={}", key.name(), value));
            }
        }
        parts.join("/")
    }

    /// Convert to Hive-style path string without scheme validation.
    ///
    /// This is a convenience method for display purposes. It sorts keys
    /// alphabetically and formats them as "key=value/key=value".
    pub fn to_path_string(&self) -> String {
        let mut pairs: Vec<_> = self.0.iter().collect();
        pairs.sort_by_key(|(k, _)| k.as_str());
        pairs
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Parse from Hive-style path.
    pub fn from_path(path: &str) -> Result<Self> {
        let mut values = HashMap::new();
        for part in path.split('/') {
            if part.is_empty() {
                continue;
            }
            let Some((key, value)) = part.split_once('=') else {
                return Err(Error::InvalidPartition(format!(
                    "Invalid partition path segment: '{}'",
                    part
                )));
            };
            // Always store as string - type conversion happens during validation
            values.insert(key.to_string(), PartitionValue::String(value.to_string()));
        }
        Ok(Self(values))
    }
}

impl Default for PartitionValues {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> From<[(&str, &str); N]> for PartitionValues {
    fn from(arr: [(&str, &str); N]) -> Self {
        let mut values = HashMap::new();
        for (k, v) in arr {
            values.insert(k.to_string(), PartitionValue::String(v.to_string()));
        }
        Self(values)
    }
}

impl<const N: usize> From<[(&str, i64); N]> for PartitionValues {
    fn from(arr: [(&str, i64); N]) -> Self {
        let mut values = HashMap::new();
        for (k, v) in arr {
            values.insert(k.to_string(), PartitionValue::Int(v));
        }
        Self(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_partition_scheme_builder() {
        let scheme = PartitionScheme::new()
            .add_string("channel")
            .add_date("date");

        assert_eq!(scheme.keys.len(), 2);
        assert_eq!(scheme.keys[0].name(), "channel");
        assert_eq!(scheme.keys[1].name(), "date");
    }

    #[test]
    fn test_partition_values_to_path() {
        let scheme = PartitionScheme::new()
            .add_string("channel")
            .add_date("date");

        let values: PartitionValues = [("channel", "101"), ("date", "2026-01-29")].into();

        let path = values.to_path(&scheme);
        assert_eq!(path, "channel=101/date=2026-01-29");
    }

    #[test]
    fn test_partition_values_from_path() {
        let path = "channel=101/date=2026-01-29";
        let values = PartitionValues::from_path(path).unwrap();

        assert_eq!(
            values.get("channel"),
            Some(&PartitionValue::String("101".to_string()))
        );
        assert_eq!(
            values.get("date"),
            Some(&PartitionValue::String("2026-01-29".to_string()))
        );
    }

    #[test]
    fn test_partition_values_int() {
        let values: PartitionValues = [("shard", 5i64)].into();
        assert_eq!(values.get("shard"), Some(&PartitionValue::Int(5)));
    }

    #[test]
    fn test_partition_scheme_validate() {
        let scheme = PartitionScheme::new()
            .add_string("channel")
            .add_int("shard");

        // Valid values
        let mut values = PartitionValues::new();
        values.insert("channel", "101");
        values.insert("shard", 5i64);
        assert!(scheme.validate(&values).is_ok());

        // Missing key
        let values: PartitionValues = [("channel", "101")].into();
        assert!(scheme.validate(&values).is_err());

        // Extra key
        let mut values = PartitionValues::new();
        values.insert("channel", "101");
        values.insert("shard", 5i64);
        values.insert("extra", "bad");
        assert!(scheme.validate(&values).is_err());
    }
}
