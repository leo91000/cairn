//! Versioned content identities. New blocks use BLAKE3; published SHA-256 blocks
//! remain readable until the generations referring to them are collected.

pub fn block(bytes: &[u8]) -> String {
    format!("b3-{}", blake3::hash(bytes).to_hex())
}

pub fn valid(identity: &str) -> bool {
    let hex = identity.strip_prefix("b3-").unwrap_or(identity);
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn matches(identity: &str, bytes: &[u8]) -> bool {
    if !valid(identity) {
        return false;
    }
    if let Some(expected) = identity.strip_prefix("b3-") {
        return blake3::hash(bytes).to_hex().as_str() == expected;
    }
    hex::encode(aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, bytes).as_ref()) == identity
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn algorithm_is_explicit_and_both_formats_verify_their_bytes() {
        let new = block(b"abc");
        let legacy = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(new.starts_with("b3-"));
        for identity in [new.as_str(), legacy] {
            assert!(valid(identity));
            assert!(matches(identity, b"abc"));
            assert!(!matches(identity, b"abd"));
        }
        assert!(!matches(new.strip_prefix("b3-").unwrap(), b"abc"));
        for invalid in ["../block", "b4-", "b3-", "B3-", ""] {
            assert!(!valid(invalid));
        }
    }
}
