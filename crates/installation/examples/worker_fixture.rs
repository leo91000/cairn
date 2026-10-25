//! Test-only worker executable with synthetic authority, or the real Beacon for
//! browser journeys which replace only the external provider's HTTP transport.
//! No browser hosting, local login or OAuth endpoints are added.
#[allow(dead_code)]
#[path = "../src/main.rs"]
mod native;
#[path = "../tests/common/relay_fixture.rs"]
#[allow(dead_code)]
mod relay_fixture;

async fn router(
    service: std::sync::Arc<cairn_installation::service::Service>,
) -> cairn_installation::error::Result<axum::Router> {
    // Slack's end-to-end fixture runs the real manager and Luau workers while
    // keeping all provider traffic on its synthetic loopback HTTP server.
    if let Ok(origin) = std::env::var("CAIRN_FIXTURE_PLUGIN_HTTP_ORIGIN") {
        let url = url::Url::parse(&origin)
            .map_err(|_| cairn_installation::error::Error::bad("Invalid fixture origin."))?;
        if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") {
            return Err(cairn_installation::error::Error::bad(
                "Fixture origin must be loopback HTTP.",
            ));
        }
        service.plugins.route_http_to(Some(origin));
    }
    if let Ok(origin) = std::env::var("CAIRN_FIXTURE_PLUGIN_SOCKET_ORIGIN") {
        let url = url::Url::parse(&origin)
            .map_err(|_| cairn_installation::error::Error::bad("Invalid fixture socket origin."))?;
        if !matches!(url.scheme(), "ws" | "wss") || url.host_str() != Some("127.0.0.1") {
            return Err(cairn_installation::error::Error::bad(
                "Fixture socket origin must be loopback WS or WSS.",
            ));
        }
        if url.scheme() == "wss"
            && let Ok(ca) = std::env::var("CAIRN_FIXTURE_PLUGIN_SOCKET_CA")
        {
            let der = std::fs::read(ca)?;
            let mut roots = rustls::RootCertStore::empty();
            roots
                .add(rustls::pki_types::CertificateDer::from(der))
                .map_err(|_| cairn_installation::error::Error::bad("Invalid fixture CA."))?;
            let client = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            service
                .plugins
                .route_sockets_with_tls_to(origin, std::sync::Arc::new(client));
        } else {
            service.plugins.route_sockets_to(Some(origin));
        }
    }
    if std::env::var("CAIRN_FIXTURE_BEACON_AUTHORITY").as_deref() == Ok("true") {
        cairn_installation::http::router(service).await
    } else {
        relay_fixture::router(service).await
    }
}

fn main() -> std::process::ExitCode {
    native::main_with_router(router)
}
