//! Process-wide TLS setup for outbound connections such as the Beacon relay.

/// Selects aws-lc-rs as the process rustls provider.
///
/// The workspace compiles both rustls providers, so rustls cannot pick one by itself and panics
/// when a client config is built without an explicit provider. Call this before any TLS client
/// exists; later calls are harmless.
pub fn install_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}
