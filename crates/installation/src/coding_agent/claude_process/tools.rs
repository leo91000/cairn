use super::*;

pub(super) fn allowed_tool(plan: &Value, name: &str, input: &Value) -> bool {
    if plan["sandbox"] == "yolo" {
        return true;
    }
    let read_only = matches!(
        name,
        "Bash" | "Read" | "Glob" | "Grep" | "WebSearch" | "WebFetch" | "TodoWrite"
    );
    if read_only || name.starts_with("mcp__") {
        return true;
    }
    let writes = matches!(name, "Write" | "Edit" | "NotebookEdit");
    if plan["sandbox"] != "workspace-write" || !writes {
        return false;
    }
    let key = if name == "NotebookEdit" {
        "notebook_path"
    } else {
        "file_path"
    };
    let Some(path) = resolved(Path::new(text(input, key))) else {
        return false;
    };
    strings(&plan["writableRoots"]).any(|root| path.starts_with(root))
}

/// The callback of a plan turn: reading, searching, the sandboxed shell, MCP tools
/// already restricted to read-only ones, and Claude Code's own plan file.
pub(super) fn plan_tool(plan: &Value, plans: &Path, name: &str, input: &Value) -> bool {
    match name {
        "Read" | "Glob" | "Grep" | "WebSearch" | "WebFetch" | "TodoWrite" => true,
        // The shell runs in the read-only sandbox without network, never outside it, and
        // a command cannot widen the network allowlist for itself.
        "Bash" => {
            input["dangerouslyDisableSandbox"] != true && input.get("allowed_domains").is_none()
        }
        "Write" | "Edit" | "NotebookEdit" => {
            let key = if name == "NotebookEdit" {
                "notebook_path"
            } else {
                "file_path"
            };
            let Some(plans) = plan_directory(plans) else {
                return false;
            };
            resolved(Path::new(text(input, key))).is_some_and(|path| path.starts_with(plans))
        }
        _ if name.starts_with("mcp__") => strings(&plan["claudePlanTools"]).any(|allowed| {
            allowed
                .strip_suffix('*')
                .map_or(allowed == name, |prefix| name.starts_with(prefix))
        }),
        _ => false,
    }
}

/// Canonical plan storage is trusted only when every path component is a real directory,
/// not a symlink that can redirect the native plan-file write into the workspace.
pub(super) fn plan_directory(plans: &Path) -> Option<PathBuf> {
    let absolute = if plans.is_absolute() {
        plans.to_owned()
    } else {
        std::env::current_dir().ok()?.join(plans)
    };
    let mut current = PathBuf::new();
    for component in absolute.components() {
        current.push(component.as_os_str());
        if !matches!(component, Component::Normal(_)) {
            continue;
        }

        let metadata = std::fs::symlink_metadata(&current).ok()?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return None;
        }
    }

    std::fs::canonicalize(absolute).ok()
}

/// `path` with its symlinks resolved, or its parent's when the file does not exist yet.
pub(super) fn resolved(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok().or_else(|| {
        let parent = std::fs::canonicalize(path.parent()?).ok()?;
        Some(parent.join(path.file_name().unwrap_or_default()))
    })
}

#[cfg(test)]
mod side_question_tests {
    use super::answers_side_questions;

    #[cfg(unix)]
    #[tokio::test]
    async fn finishing_side_cancellation_never_waits_on_a_full_open_control_pipe() {
        use std::{os::fd::AsRawFd, process::Stdio, task::Poll};

        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let fd = stdin.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0 && flags & libc::O_NONBLOCK != 0);
        // The child keeps its read end open but never reads it. Fill even the
        // final partial page so no cancellation frame can fit atomically.
        let mut filled = 0;
        for size in [libc::PIPE_BUF, 1] {
            let bytes = vec![b'x'; size];
            loop {
                let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
                if written < 0 {
                    assert_eq!(
                        std::io::Error::last_os_error().kind(),
                        std::io::ErrorKind::WouldBlock
                    );
                    break;
                }
                filled += written;
            }
        }

        let mut all_ready = true;
        for index in 0..5 {
            let request = format!("side-question-{index}");
            let cancellation = super::cancel_side_request(&mut stdin, &request);
            tokio::pin!(cancellation);
            all_ready &= matches!(futures_util::poll!(&mut cancellation), Poll::Ready(()));
        }
        child.start_kill().unwrap();
        child.wait().await.unwrap();
        assert!(filled > 0);
        assert!(
            all_ready,
            "Main completion must not wait for side-control readiness or a deadline"
        );
    }

    #[test]
    fn only_the_qualified_release_answers_side_questions() {
        assert!(answers_side_questions("2.1.308"));
        for release in [
            "2.1.280",
            "2.1.307",
            "2.1.307-rc",
            "2.1.308-rc.1",
            "2.1.308+build",
            "2.1.310",
            "2.1.310-beta.1",
            "2.2.0",
            "3.0.0+build",
            "2.1",
            "",
            "next",
        ] {
            assert!(!answers_side_questions(release), "{release}");
        }
    }
}
