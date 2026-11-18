//! Stable identifier helpers shared by the backend and the UI.
//!
//! Task, schedule, and command IDs are ASCII slugs:
//! `[a-z][a-z0-9_-]{0,63}`. They are immutable after creation and are also used
//! as file names, so the character set is deliberately conservative.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_ID_LEN: usize = 64;

/// Returns true when `value` is a valid identifier.
pub fn is_valid_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_ID_LEN {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..].iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-'
    })
}

/// Converts a display name into a candidate identifier.
///
/// Non-ASCII names have no useful ASCII form, so the caller should fall back to
/// [`generated_id`] when the result is empty.
pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            let character = character.to_ascii_lowercase();
            // A slug must start with a letter. A leading digit falls back to
            // generated_id() so the result never needs a special prefix.
            if slug.is_empty() && !character.is_ascii_lowercase() {
                break;
            }
            slug.push(character);
            last_dash = false;
        } else if !slug.is_empty() && !last_dash {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= MAX_ID_LEN {
            break;
        }
    }
    slug.trim_end_matches('-').to_string()
}

/// Returns a unique fallback identifier such as `task-1a2b3c4d`.
pub fn generated_id(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let mut value = nanos ^ count.rotate_left(17);
    // A small xorshift keeps the suffix well distributed without extra crates.
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    format!("{prefix}-{:08x}", value as u32)
}

/// Returns a valid, unique candidate based on `name`, or a generated one.
pub fn id_from_name(prefix: &str, name: &str) -> String {
    let slug = slugify(name);
    if is_valid_id(&slug) {
        slug
    } else {
        generated_id(prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_and_rejects_ids() {
        for value in ["a", "task", "evening-lights", "a_1", "z9"] {
            assert!(is_valid_id(value), "{value}");
        }
        for value in [
            "",
            "A",
            "1task",
            "task space",
            "task.report",
            &"a".repeat(65),
        ] {
            assert!(!is_valid_id(value), "{value}");
        }
    }

    #[test]
    fn slugifies_names() {
        assert_eq!(slugify("Evening lights"), "evening-lights");
        assert_eq!(slugify("  Winter / porch  "), "winter-porch");
        assert_eq!(slugify("Z-Wave"), "z-wave");
        assert_eq!(slugify("1st floor"), "");
        assert_eq!(slugify("Привет"), "");
    }

    #[test]
    fn falls_back_for_non_ascii_names() {
        let id = id_from_name("task", "Привет");
        assert!(id.starts_with("task-"));
        assert!(is_valid_id(&id));
    }
}
