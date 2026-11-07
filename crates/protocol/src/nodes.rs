//! Execution nodes added without a manager address. Beacon introduces a node
//! and its manager, which then connect directly over WebRTC when the networks
//! allow it. Otherwise Beacon carries TLS between them. Both paths end on the
//! manager's node certificate, which the node pins from its install command.

/// One reliable, ordered DataChannel per HTTP connection of the node.
pub const DATA_CHANNEL: &str = "cairn.node.v1";
/// The channel the node opens with its offer, kept open with the connection.
pub const CONTROL_CHANNEL: &str = "cairn.node.control";
/// A complete offer or answer, candidates included.
pub const MAX_SDP: usize = 16_384;
/// Direct node connections one installation keeps at once.
pub const MAX_PEERS: usize = 32;

/// The pin of a DER certificate: its SHA-256 digest in lowercase hex.
pub fn certificate_pin(der: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, der)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn valid_pin(pin: &str) -> bool {
    pin.len() == 64
        && pin
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The SDP fingerprint attribute value for a pin, as WebRTC writes it.
pub fn sdp_fingerprint(pin: &str) -> String {
    let pairs: Vec<_> = pin
        .as_bytes()
        .chunks(2)
        .map(|pair| String::from_utf8_lossy(pair).to_ascii_uppercase())
        .collect();
    format!("sha-256 {}", pairs.join(":"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_are_lowercase_sha256_hex() {
        let pin = certificate_pin(b"certificate");
        assert!(valid_pin(&pin));
        assert!(!valid_pin(&pin.to_ascii_uppercase()));
        assert!(!valid_pin(&pin[1..]));
        assert_eq!(
            sdp_fingerprint(&pin),
            format!(
                "sha-256 {}",
                pin.as_bytes()
                    .chunks(2)
                    .map(|pair| std::str::from_utf8(pair).unwrap().to_ascii_uppercase())
                    .collect::<Vec<_>>()
                    .join(":")
            )
        );
        assert!(crate::direct::valid_fingerprint(&sdp_fingerprint(&pin)));
    }
}
