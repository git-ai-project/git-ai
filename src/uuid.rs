pub fn generate_v4() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("getrandom failed");

    // Set version to 4
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    // Set variant to RFC4122
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

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
        assert_eq!(uuid.len(), 36);
        assert_eq!(&uuid[8..9], "-");
        assert_eq!(&uuid[13..14], "-");
        assert_eq!(&uuid[18..19], "-");
        assert_eq!(&uuid[23..24], "-");
        let version_char = uuid.chars().nth(14).unwrap();
        assert_eq!(version_char, '4');
        let variant_char = uuid.chars().nth(19).unwrap();
        assert!(matches!(variant_char, '8' | '9' | 'a' | 'b'));
    }

    #[test]
    fn test_generate_v4_uniqueness() {
        let mut uuids = std::collections::HashSet::new();
        for _ in 0..1000 {
            let uuid = generate_v4();
            assert!(uuids.insert(uuid), "Generated duplicate UUID");
        }
    }

    #[test]
    fn test_generate_v4_lowercase() {
        let uuid = generate_v4();
        for ch in uuid.chars() {
            assert!(ch.is_ascii_hexdigit() || ch == '-');
            if ch.is_alphabetic() {
                assert!(ch.is_lowercase());
            }
        }
    }
}
