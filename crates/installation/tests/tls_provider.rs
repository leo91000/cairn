//! The installation reaches Beacon over public HTTPS/WSS. Its dependency graph compiles more than
//! one rustls provider, so the process must choose one before building any TLS client.

#[test]
fn outbound_tls_clients_have_a_process_crypto_provider() {
    cairn_installation::tls::install_provider();

    let _ = rustls::ClientConfig::builder();
}
