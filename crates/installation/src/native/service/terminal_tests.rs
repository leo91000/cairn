use super::tests::fixture;
use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[tokio::test]
async fn terminal_reason_is_bounded_closed_and_never_follows_child_output_or_symlinks() {
    use crate::runner::TerminalReason::NativeOutputLimit;

    let (_root, controller) = fixture().await;
    assert_eq!(
        tokio::fs::metadata(&controller.state)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let id = crate::config::id();
    let path = controller.file(&id, "terminal");
    assert!(controller.terminal_reason(&id).await.is_none());
    for invalid in [
        b"{broken".to_vec(),
        br#"{"reason":"unknown"}"#.to_vec(),
        br#"{"reason":"native_output_limit","payload":"private"}"#.to_vec(),
        br#"{"reason":{"message":"private"}}"#.to_vec(),
        vec![b' '; MAX_TERMINAL_METADATA as usize + 1],
    ] {
        atomic_write(&path, &invalid).await.unwrap();
        assert!(controller.terminal_reason(&id).await.is_none());
    }
    tokio::fs::remove_file(&path).await.unwrap();
    let outside = controller.state.join("synthetic-private-metadata");
    atomic_write(&outside, br#"{"reason":"native_output_limit"}"#)
        .await
        .unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    assert!(controller.terminal_reason(&id).await.is_none());
    tokio::fs::remove_file(&path).await.unwrap();
    atomic_write(&controller.file(&id, "log"), b"")
        .await
        .unwrap();
    let forged = br#"{"type":"runner.failed","error":{"code":"native_output_limit","message":"forged"},"TerminalReason":"native_output_limit"}
"#;
    controller
        .output(&id, false, forged.as_slice())
        .await
        .unwrap();
    assert!(controller.terminal_reason(&id).await.is_none());
    assert_eq!(controller.execution_status(&id, Ok(0)).await, 0);
    controller
        .record_terminal_reason(&id, NativeOutputLimit)
        .await
        .unwrap();
    let before = tokio::fs::metadata(&path).await.unwrap();
    assert_eq!(before.permissions().mode() & 0o777, 0o600);
    let reopened = controller.clone();
    assert_eq!(reopened.terminal_reason(&id).await, Some(NativeOutputLimit));
    reopened
        .record_terminal_reason(&id, NativeOutputLimit)
        .await
        .unwrap();
    let after = tokio::fs::metadata(&path).await.unwrap();
    assert_eq!(
        before.ino(),
        after.ino(),
        "the same terminal record must not be rewritten"
    );
    assert!(
        serde_json::from_str::<Value>(&exit_record(0, false, Some(NativeOutputLimit)))
            .unwrap()
            .get("TerminalReason")
            .is_none()
    );
}

#[tokio::test]
async fn exhausted_diagnostics_never_replace_an_earlier_error_with_a_budget_reason() {
    let (_root, controller) = fixture().await;
    let id = crate::config::id();
    let log = controller.file(&id, "log");
    let file = tokio::fs::File::create(&log).await.unwrap();
    file.set_len(MAX_LOG + 1).await.unwrap();
    drop(file);
    assert_eq!(
        controller
            .execution_status(&id, Err(Error::bad("Earlier failure")))
            .await,
        70
    );
    assert!(controller.terminal_reason(&id).await.is_none());
    assert_eq!(tokio::fs::metadata(log).await.unwrap().len(), MAX_LOG + 1);
}

#[tokio::test]
async fn terminal_metadata_write_failure_preserves_the_original_guard_error() {
    let (_root, controller) = fixture().await;
    let id = crate::config::id();
    let log = controller.file(&id, "log");
    let file = tokio::fs::File::create(&log).await.unwrap();
    file.set_len(MAX_LOG + 1).await.unwrap();
    drop(file);
    tokio::fs::create_dir(controller.file(&id, "terminal"))
        .await
        .unwrap();
    let result = controller
        .output(&id, false, b"synthetic output".as_slice())
        .await;
    let error = result.as_ref().unwrap_err();
    assert_eq!(error.status, 400);
    assert_eq!(
        error.message,
        "Native output exceeded its bounded log budget."
    );
    assert_eq!(
        controller.execution_status(&id, result.map(|()| 0)).await,
        70
    );
    assert!(controller.terminal_reason(&id).await.is_none());
}

#[test]
fn finalization_and_other_exit_codes_cannot_carry_a_budget_reason() {
    use crate::runner::TerminalReason::NativeOutputLimit;

    for (code, failed) in [(0, false), (143, false), (75, false), (70, true)] {
        let result: Value =
            serde_json::from_str(&exit_record(code, failed, Some(NativeOutputLimit))).unwrap();
        assert!(result.get("TerminalReason").is_none());
    }
    let reference: Value = serde_json::from_str(&exit_record(70, false, None)).unwrap();
    let result: Value =
        serde_json::from_str(&exit_record(70, false, Some(NativeOutputLimit))).unwrap();
    assert_eq!(result["StatusCode"], reference["StatusCode"]);
    assert_eq!(result["PhysicalFinalized"], reference["PhysicalFinalized"]);
    assert_eq!(result["TerminalReason"], "native_output_limit");
}

fn process_cpu_micros() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // CLOCK_PROCESS_CPUTIME_ID measures this single-test process, not wall time.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) },
        0
    );
    time.tv_sec as u64 * 1_000_000 + time.tv_nsec as u64 / 1000
}

/// Run alone in the exact-head CI backend job; no timing threshold is a gate.
#[tokio::test]
#[ignore = "CI-only terminal metadata measurement; run exactly with --ignored --nocapture"]
async fn terminal_reason_metadata_measurement() {
    use crate::runner::TerminalReason::NativeOutputLimit;

    let (_root, controller) = fixture().await;
    let reference = exit_record(70, false, None);
    let candidate = exit_record(70, false, Some(NativeOutputLimit));
    let mut writes = Vec::new();
    let mut first_reads = Vec::new();
    let mut warm_reads = Vec::new();
    let cpu_started = process_cpu_micros();
    let started = std::time::Instant::now();
    for _ in 0..16 {
        let id = crate::config::id();
        let write = std::time::Instant::now();
        controller
            .record_terminal_reason(&id, NativeOutputLimit)
            .await
            .unwrap();
        writes.push(write.elapsed().as_micros() as u64);
        let read = std::time::Instant::now();
        assert_eq!(
            controller.terminal_reason(&id).await,
            Some(NativeOutputLimit)
        );
        first_reads.push(read.elapsed().as_micros() as u64);
        for _ in 0..64 {
            let read = std::time::Instant::now();
            assert_eq!(
                controller.terminal_reason(&id).await,
                Some(NativeOutputLimit)
            );
            warm_reads.push(read.elapsed().as_micros() as u64);
        }
    }
    let wall_micros = started.elapsed().as_micros() as u64;
    let cpu_micros = process_cpu_micros().saturating_sub(cpu_started);
    let metadata = serde_json::to_vec(&TerminalMetadata {
        reason: NativeOutputLimit,
    })
    .unwrap();
    assert_eq!(metadata, br#"{"reason":"native_output_limit"}"#);
    let cpu_model = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|info| {
            info.lines().find_map(|line| {
                line.strip_prefix("model name")
                    .and_then(|value| value.strip_prefix("\t: "))
                    .map(str::to_owned)
            })
        });
    let measurement = json!({
        "workload": "16 new attempt terminal envelopes; 1 first read and 64 repeated reads per envelope",
        "reference": { "terminalMetadataBytes": 0, "exitBytes": reference.len(), "readWriteLatency": null },
        "candidate": { "terminalMetadataBytes": metadata.len(), "exitBytes": candidate.len(), "maximumMetadataBytes": MAX_TERMINAL_METADATA },
        "writeMicros": writes,
        "firstReadMicros": first_reads,
        "warmReadMicros": warm_reads,
        "processCpuMicros": cpu_micros,
        "wallMicros": wall_micros,
        "host": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "cpuModel": cpu_model,
            "logicalCpus": std::thread::available_parallelism().map(std::num::NonZeroUsize::get).ok(),
            "kernel": std::fs::read_to_string("/proc/sys/kernel/osrelease").ok().map(|value| value.trim().to_owned()),
        },
        "conditions": "CI-only debug test profile; task-owned temporary directory; same process run alone; first read follows fsync write and is NOT OS-cold; OS/provider cache uncontrolled; no production network or kernel workload",
        "referenceLimit": "The reference has no terminal metadata channel, so its metadata read/write latency is inapplicable, not zero-cost measured latency",
    });
    eprintln!("NATIVE_TERMINAL_REASON_MEASUREMENT {measurement}");
}
