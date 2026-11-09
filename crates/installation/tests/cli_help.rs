//! Exercise help through the shipped executable, without installation credentials.
use std::{
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn bounded_output(command: &mut Command) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("Help did not exit; it may have started a service: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}

fn cairn(args: &[&str]) -> Output {
    bounded_output(
        Command::new(env!("CARGO_BIN_EXE_cairn"))
            .args(args)
            .env_clear()
            .env("CAIRN_MCP_ORIGIN", "invalid-origin-must-not-be-used"),
    )
}

fn help(args: &[&str]) -> String {
    let output = cairn(args);
    assert!(output.status.success(), "{args:?}: {output:?}");
    assert!(output.stderr.is_empty(), "{args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn general_help_is_successful_and_lists_plugin_discovery() {
    let expected = help(&["--help"]);
    for alias in [vec!["-h"], vec!["help"]] {
        assert_eq!(help(&alias), expected);
    }
    for text in ["Usage:", "serve", "claim", "plugin", "--list", "cairn help"] {
        assert!(expected.contains(text), "missing {text}: {expected}");
    }
}

#[test]
fn command_help_never_starts_services_or_requires_a_control_socket() {
    for command in [
        "serve",
        "claim",
        "rotate-token",
        "node-enroll",
        "node-daemon",
        "node-connect",
        "chat",
        "plugin",
        "guest",
        "native-controller",
        "native-enclave",
        "native-docker",
        "native-docker-reset",
        "native-watchdog",
        "toolkit-env",
        "runner-broker",
        "runner-client",
        "runner-entry",
        "codex-service",
        "supervise",
        "plugin-worker",
        "mcp-code-worker",
        "version",
    ] {
        let expected = help(&[command, "--help"]);
        assert!(expected.contains("Usage:"), "{command}: {expected}");
        assert_eq!(help(&[command, "-h"]), expected);
        assert_eq!(help(&["help", command]), expected);
    }
    let plugin = help(&["plugin"]);
    assert!(plugin.contains("--list"));
    assert!(plugin.contains("--upload"));
    assert!(plugin.contains("secret"));
    for operation in ["--upload", "--download", "--delete-file"] {
        assert!(help(&["plugin", "example", operation, "--help"]).contains(operation));
        assert_eq!(
            help(&["help", "plugin", "example", operation]),
            help(&["plugin", "example", operation, "-h"])
        );
    }
    assert!(help(&["plugin", "--list", "--help"]).contains("--list"));
}

#[test]
fn invalid_help_topics_and_commands_fail_with_a_recovery_hint() {
    for args in [
        vec!["not-a-command"],
        vec!["help", "not-a-command"],
        vec!["help", "serve", "unexpected"],
    ] {
        let output = cairn(&args);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("cairn --help")
        );
    }
}

#[test]
fn help_after_local_arguments_does_not_read_configuration_or_start_work() {
    for args in [
        vec!["serve", "ignored", "--help"],
        vec!["serve", "--help", "ignored"],
        vec!["node-daemon", "/private/node", "--help"],
        vec![
            "node-enroll",
            "https://manager.example",
            "/private/node",
            "-h",
        ],
        vec!["claim", "ignored", "--help"],
    ] {
        let output = bounded_output(
            Command::new(env!("CARGO_BIN_EXE_cairn"))
                .args(&args)
                .env_clear()
                .env("PORT", "invalid-configuration"),
        );
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert!(String::from_utf8(output.stdout).unwrap().contains("Usage:"));
    }
    let output = cairn(&["supervise", "/bin/true", "--help"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("control socket")
    );
}

#[test]
fn plugin_metadata_requires_an_active_run_instead_of_claiming_missing_commands() {
    for args in [
        vec!["plugin", "--list"],
        vec!["plugin", "onepassword", "--help"],
        vec!["help", "plugin", "onepassword", "read"],
    ] {
        let output = bounded_output(
            Command::new(env!("CARGO_BIN_EXE_cairn"))
                .args(&args)
                .env_clear(),
        );
        assert!(!output.status.success());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("active Cairn conversation"),
            "{args:?}"
        );
    }
}
