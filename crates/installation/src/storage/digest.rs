//! Content identities for immutable blocks.

pub fn block(bytes: &[u8]) -> String {
    format!("b3-{}", blake3::hash(bytes).to_hex())
}

pub fn valid(identity: &str) -> bool {
    let Some(hex) = identity.strip_prefix("b3-") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn matches(identity: &str, bytes: &[u8]) -> bool {
    valid(identity) && block(bytes) == identity
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_verifies_its_bytes_and_requires_the_algorithm() {
        let identity = block(b"abc");
        assert!(valid(&identity));
        assert!(matches(&identity, b"abc"));
        assert!(!matches(&identity, b"abd"));
        for invalid in [
            "../block",
            "b4-",
            "b3-",
            "B3-",
            "",
            identity.strip_prefix("b3-").unwrap(),
        ] {
            assert!(!valid(invalid));
            assert!(!matches(invalid, b"abc"));
        }
    }
}
