//! Run tests that must own process-global controller state in a fresh process.

pub(crate) fn run_ignored(name: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed; 0 ignored"),
        "isolated test {name} did not pass exactly once:\n{stdout}\n{stderr}"
    );
}
