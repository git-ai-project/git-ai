//! Simple UUID v4 generation
//!
//! This module provides a minimal UUID v4 implementation using the existing
//! rand dependency. We only need random UUIDs, so we don't need a full UUID
//! library with parsing, other versions, etc.

/// Generate a random UUID v4 (RFC 4122).
///
/// Format: xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx
/// where x is any hexadecimal digit and y is one of 8, 9, A, or B.
///
/// # Example
/// ```
/// let id = git_ai::uuid::generate_v4();
/// assert_eq!(id.len(), 36); // 32 hex digits + 4 hyphens
/// ```
pub fn generate_v4() -> String {
    use rand::RngExt;

    let mut rng = rand::rng();
    let mut bytes: [u8; 16] = rng.random();

    // Set version to 4 (bits 12-15 of time_hi_and_version)
    bytes[6] = (bytes[6] & 0x0f) | 0x40;

    // Set variant to RFC4122 (bits 6-7 of clock_seq_hi_and_reserved)
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    // Format as hyphenated lowercase hex string
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_v4_format() {
        let uuid = generate_v4();

        // Check length: 32 hex digits + 4 hyphens = 36 chars
        assert_eq!(uuid.len(), 36);

        // Check hyphen positions
        assert_eq!(&uuid[8..9], "-");
        assert_eq!(&uuid[13..14], "-");
        assert_eq!(&uuid[18..19], "-");
        assert_eq!(&uuid[23..24], "-");

        // Check version nibble (should be 4)
        let version_char = uuid.chars().nth(14).unwrap();
        assert_eq!(version_char, '4');

        // Check variant bits (should be 8, 9, a, or b)
        let variant_char = uuid.chars().nth(19).unwrap();
        assert!(matches!(variant_char, '8' | '9' | 'a' | 'b'));
    }

    #[test]
    fn test_generate_v4_uniqueness() {
        // Generate multiple UUIDs and ensure they're different
        let mut uuids = std::collections::HashSet::new();
        for _ in 0..1000 {
            let uuid = generate_v4();
            assert!(uuids.insert(uuid), "Generated duplicate UUID");
        }
    }

    #[test]
    fn test_generate_v4_lowercase() {
        let uuid = generate_v4();
        // Should be lowercase hex
        for ch in uuid.chars() {
            assert!(ch.is_ascii_hexdigit() || ch == '-');
            if ch.is_alphabetic() {
                assert!(ch.is_lowercase());
            }
        }
    }
}
