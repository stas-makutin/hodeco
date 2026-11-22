//! Deterministic symmetric jitter shared by schedules and task start delays.

use chrono::TimeDelta;
use std::time::Duration;

/// Returns a deterministic offset in `[-jitter, +jitter]` milliseconds.
///
/// The offset is derived from a caller-provided seed, a base timestamp, and an
/// attempt counter. Callers must clamp the result when the surrounding range
/// has a lower bound.
pub fn offset(seed: &[u8], base_nanos: i64, attempt: u32, jitter: Duration) -> TimeDelta {
    let millis = jitter.as_millis() as i64;
    if millis <= 0 {
        return TimeDelta::zero();
    }
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in seed {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    for byte in base_nanos.to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    for byte in attempt.to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let span = (millis * 2 + 1) as u64;
    let offset = (hash % span) as i64 - millis;
    TimeDelta::milliseconds(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_is_deterministic_and_bounded() {
        let jitter = Duration::from_secs(10);
        let first = offset(b"test", 12345, 0, jitter);
        let second = offset(b"test", 12345, 0, jitter);
        assert_eq!(first, second);
        assert!(first >= TimeDelta::seconds(-10));
        assert!(first <= TimeDelta::seconds(10));
    }
}
