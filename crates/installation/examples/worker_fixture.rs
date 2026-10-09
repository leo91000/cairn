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
    relay_fixture::router(service).await
}

fn main() -> std::process::ExitCode {
    native::main_with_router(router)
}
