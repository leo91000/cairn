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
    #[cfg(feature = "worker-fixture-hooks")]
    if std::env::args().nth(1).as_deref() == Some("--fixture-hooks-capability") {
        println!("cairn-worker-fixture-hooks-v1");
        return std::process::ExitCode::SUCCESS;
    }
    #[cfg(feature = "worker-fixture-hooks")]
    if let Some(path) = std::env::var_os("CAIRN_FIXTURE_CLOCK_FILE") {
        cairn_installation::config::set_fixture_clock(path.into());
    }
    native::main_with_router(router)
}
