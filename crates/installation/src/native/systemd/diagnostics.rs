//! Best-effort observations after fencing has already refused completion.
//! None of these values establish ownership or permit releasing a workload.
use std::{
    path::Path,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;

#[derive(Default)]
struct Window {
    reported_at: Option<Instant>,
    observations: u64,
}

impl Window {
    fn admit(&mut self, now: Instant) -> Option<u64> {
        self.observations = self.observations.saturating_add(1);
        if self
            .reported_at
            .is_some_and(|reported| now.duration_since(reported) < Duration::from_secs(30))
        {
            return None;
        }
        self.reported_at = Some(now);
        Some(std::mem::take(&mut self.observations))
    }
}

static WINDOW: LazyLock<Mutex<Window>> = LazyLock::new(Mutex::default);

struct Report<'a> {
    state: &'a str,
    stop_succeeded: bool,
    populated: bool,
    frozen: Option<bool>,
    cgroup_available: bool,
    tasks: Option<u64>,
    observations: u64,
}

pub(super) fn frozen(events: &str) -> Option<bool> {
    events.lines().find_map(|line| match line {
        "frozen 0" => Some(false),
        "frozen 1" => Some(true),
        _ => None,
    })
}

fn active_state(state: &str) -> &'static str {
    match state {
        "active" => "active",
        "inactive" => "inactive",
        "failed" => "failed",
        "activating" => "activating",
        "deactivating" => "deactivating",
        "reloading" => "reloading",
        "maintenance" => "maintenance",
        _ => "unknown",
    }
}

async fn task_count(group: &Path) -> Option<u64> {
    let file = tokio::fs::File::open(group.join("pids.current"))
        .await
        .ok()?;
    let mut value = String::new();
    // The kernel counter is a u64 plus newline. Reject truncated/malformed input.
    file.take(32).read_to_string(&mut value).await.ok()?;
    if value.len() == 32 {
        return None;
    }
    value.trim().parse().ok()
}

pub(super) async fn record(
    state: &str,
    stop_succeeded: bool,
    populated: bool,
    frozen: Option<bool>,
    group: Option<&Path>,
) {
    let Some(observations) = WINDOW
        .lock()
        .ok()
        .and_then(|mut window| window.admit(Instant::now()))
    else {
        return;
    };
    // Diagnostic failure or delay never changes the already-refused fence.
    let tasks = match group {
        Some(group) => tokio::time::timeout(Duration::from_millis(100), task_count(group))
            .await
            .ok()
            .flatten(),
        None => None,
    };
    emit(&Report {
        state,
        stop_succeeded,
        populated,
        frozen,
        cgroup_available: group.is_some(),
        tasks,
        observations,
    });
}

fn emit(report: &Report<'_>) {
    tracing::warn!(
        target: "cairn_performance",
        parent: None,
        operation = "native_workload_fence",
        unconfirmed_observations = report.observations,
        active_state = active_state(report.state),
        stop_succeeded = report.stop_succeeded,
        cgroup_available = report.cgroup_available,
        populated = report.populated,
        frozen_available = report.frozen.is_some(),
        frozen = report.frozen.unwrap_or(false),
        remaining_tasks_available = report.tasks.is_some(),
        remaining_tasks = report.tasks.unwrap_or(0),
        "Native workload termination remains unconfirmed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{self, Write},
        sync::Arc,
    };
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Self;

        fn make_writer(&'a self) -> Self {
            self.clone()
        }
    }

    #[test]
    fn emitted_report_excludes_private_parent_span_and_unknown_state() {
        let output = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(output.clone())
            .finish();
        let private = "synthetic-private-unit-command-and-identifier";
        tracing::dispatcher::with_default(&tracing::Dispatch::new(subscriber), || {
            let span = tracing::info_span!("workload", command = private);
            let _entered = span.enter();
            emit(&Report {
                state: private,
                stop_succeeded: true,
                populated: true,
                frozen: None,
                cgroup_available: true,
                tasks: None,
                observations: 1,
            });
        });
        let text = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(!text.contains(private));
        assert!(!text.contains("workload{"));
        assert!(text.contains("native_workload_fence"));
        assert!(text.contains("active_state=\"unknown\""));
        assert!(text.contains("remaining_tasks_available=false"));
        assert!(text.contains("frozen_available=false"));
    }

    #[test]
    fn repeated_refusals_share_a_bounded_node_report_window() {
        let mut window = Window::default();
        let now = Instant::now();
        assert_eq!(window.admit(now), Some(1));
        assert_eq!(window.admit(now + Duration::from_secs(1)), None);
        assert_eq!(window.admit(now + Duration::from_secs(29)), None);
        assert_eq!(window.admit(now + Duration::from_secs(30)), Some(3));
    }

    #[tokio::test]
    async fn missing_and_invalid_task_counters_are_not_reported_as_empty() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(task_count(directory.path()).await, None);
        for (value, expected) in [("7\n", Some(7)), ("0\n", Some(0)), ("max\n", None)] {
            tokio::fs::write(directory.path().join("pids.current"), value)
                .await
                .unwrap();
            assert_eq!(task_count(directory.path()).await, expected);
        }
        tokio::fs::write(directory.path().join("pids.current"), "1".repeat(4096))
            .await
            .unwrap();
        assert_eq!(task_count(directory.path()).await, None);
    }

    #[test]
    fn diagnostics_keep_unknown_states_distinct_without_retaining_raw_values() {
        assert_eq!(frozen("populated 1\nfrozen 0\n"), Some(false));
        assert_eq!(frozen("populated 1\nfrozen 1\n"), Some(true));
        assert_eq!(frozen("populated 1\nfrozen private-value\n"), None);
        assert_eq!(active_state("failed"), "failed");
        assert_eq!(active_state("private-value"), "unknown");
    }
}
