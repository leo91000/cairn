//! Test-only worker executable with a synthetic task-author authority.
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
    relay_fixture::router(service).await
}

fn main() -> std::process::ExitCode {
    native::main_with_router(router)
}
