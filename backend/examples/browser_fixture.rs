//! Test-only adapter for the existing installation UI journeys while #49 moves
//! that UI into the official shell. Never shipped in the application image.
#[path = "support/browser_http.rs"]
mod browser_http;
#[allow(dead_code)]
#[path = "../src/main.rs"]
mod native;

fn main() -> std::process::ExitCode {
    native::main_with_router(browser_http::router)
}
