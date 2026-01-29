/// Sequence validation for detecting gaps in message streams.
///
/// This module provides common functionality for validating monotonic sequence
/// numbers across different stream types (replay, reconstruction, etc.).
use anyhow::{bail, Result};

/// Policy for handling sequence gaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapPolicy {
    /// Fail immediately on gap detection.
    Panic,
    /// Mark as skipped but continue processing.
    Quarantine,
    /// Ignore gaps and continue.
    Ignore,
}

impl Default for GapPolicy {
    fn default() -> Self {
        Self::Panic
    }
}

/// Result of sequence validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapDetection {
    /// Sequence is monotonic (no gap).
    Sequential,
    /// Gap detected between sequences.
    Gap { from: u64, to: u64 },
}

/// Validates monotonic sequence numbers with configurable gap handling.
///
/// # Example
/// ```ignore
/// use chronicle::core::sequencer::{SequenceValidator, GapPolicy, GapDetection};
///
/// let mut validator = SequenceValidator::new(GapPolicy::Panic);
///
/// // First message - always sequential
/// assert!(matches!(validator.check(1)?, GapDetection::Sequential));
///
/// // Next message - sequential
/// assert!(matches!(validator.check(2)?, GapDetection::Sequential));
///
/// // Gap detected - fails with Panic policy
/// validator.check(5).unwrap_err();
/// ```
#[derive(Debug, Clone)]
pub struct SequenceValidator {
    last_seq: Option<u64>,
    policy: GapPolicy,
}

impl SequenceValidator {
    /// Create a new sequence validator with the given gap policy.
    pub fn new(policy: GapPolicy) -> Self {
        Self {
            last_seq: None,
            policy,
        }
    }

    /// Create a validator that panics on gaps (default).
    pub fn strict() -> Self {
        Self::new(GapPolicy::Panic)
    }

    /// Create a validator that ignores gaps.
    pub fn lenient() -> Self {
        Self::new(GapPolicy::Ignore)
    }

    /// Get the current gap policy.
    pub fn policy(&self) -> GapPolicy {
        self.policy
    }

    /// Update the gap policy.
    pub fn set_policy(&mut self, policy: GapPolicy) {
        self.policy = policy;
    }

    /// Get the last validated sequence number.
    pub fn last_seq(&self) -> Option<u64> {
        self.last_seq
    }

    /// Reset the validator state.
    pub fn reset(&mut self) {
        self.last_seq = None;
    }

    /// Check if the given sequence number is valid.
    ///
    /// Returns `GapDetection::Sequential` if the sequence is monotonic,
    /// or `GapDetection::Gap` if a gap is detected.
    ///
    /// # Errors
    /// - Returns error if gap detected and policy is `GapPolicy::Panic`
    pub fn check(&mut self, seq: u64) -> Result<GapDetection> {
        if let Some(prev) = self.last_seq {
            if seq != prev.wrapping_add(1) {
                let gap = GapDetection::Gap { from: prev, to: seq };

                match self.policy {
                    GapPolicy::Panic => {
                        bail!("sequence gap detected: {} -> {} (missing {} messages)",
                              prev, seq, seq.saturating_sub(prev).saturating_sub(1));
                    }
                    GapPolicy::Quarantine | GapPolicy::Ignore => {
                        self.last_seq = Some(seq);
                        return Ok(gap);
                    }
                }
            }
        }

        self.last_seq = Some(seq);
        Ok(GapDetection::Sequential)
    }

    /// Check sequence and return gap information without policy enforcement.
    ///
    /// This is useful when the caller wants to handle gaps differently based on context.
    pub fn check_relaxed(&mut self, seq: u64) -> GapDetection {
        if let Some(prev) = self.last_seq {
            if seq != prev.wrapping_add(1) {
                self.last_seq = Some(seq);
                return GapDetection::Gap { from: prev, to: seq };
            }
        }

        self.last_seq = Some(seq);
        GapDetection::Sequential
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sequential() {
        let mut validator = SequenceValidator::strict();

        assert_eq!(validator.check(1).unwrap(), GapDetection::Sequential);
        assert_eq!(validator.check(2).unwrap(), GapDetection::Sequential);
        assert_eq!(validator.check(3).unwrap(), GapDetection::Sequential);
        assert_eq!(validator.last_seq(), Some(3));
    }

    #[test]
    fn test_gap_panic() {
        let mut validator = SequenceValidator::new(GapPolicy::Panic);

        validator.check(1).unwrap();
        validator.check(2).unwrap();

        // Gap should cause panic
        let result = validator.check(5);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("sequence gap"));
    }

    #[test]
    fn test_gap_ignore() {
        let mut validator = SequenceValidator::new(GapPolicy::Ignore);

        validator.check(1).unwrap();
        assert_eq!(
            validator.check(5).unwrap(),
            GapDetection::Gap { from: 1, to: 5 }
        );
        assert_eq!(validator.last_seq(), Some(5));

        // Can continue after gap
        assert_eq!(validator.check(6).unwrap(), GapDetection::Sequential);
    }

    #[test]
    fn test_gap_quarantine() {
        let mut validator = SequenceValidator::new(GapPolicy::Quarantine);

        validator.check(1).unwrap();
        let detection = validator.check(5).unwrap();

        assert!(matches!(detection, GapDetection::Gap { from: 1, to: 5 }));
        assert_eq!(validator.last_seq(), Some(5));
    }

    #[test]
    fn test_reset() {
        let mut validator = SequenceValidator::strict();

        validator.check(1).unwrap();
        validator.check(2).unwrap();

        validator.reset();
        assert_eq!(validator.last_seq(), None);

        // Can start from any sequence after reset
        validator.check(100).unwrap();
        assert_eq!(validator.last_seq(), Some(100));
    }

    #[test]
    fn test_policy_change() {
        let mut validator = SequenceValidator::strict();

        validator.check(1).unwrap();

        // Change to lenient before gap
        validator.set_policy(GapPolicy::Ignore);
        validator.check(5).unwrap();  // Should not panic
    }

    #[test]
    fn test_check_relaxed() {
        let mut validator = SequenceValidator::strict();

        validator.check_relaxed(1);
        assert_eq!(validator.check_relaxed(5), GapDetection::Gap { from: 1, to: 5 });
        // No error even with strict policy
        assert_eq!(validator.last_seq(), Some(5));
    }
}
