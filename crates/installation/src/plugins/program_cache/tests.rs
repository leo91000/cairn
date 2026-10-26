use super::*;
use std::os::unix::fs::PermissionsExt;

fn executable(root: &TempDir) -> PathBuf {
    let path = root.path().join("daemon");
    std::fs::write(&path, "#!/usr/bin/python3\nimport os,socket\ns=socket.socket(socket.AF_UNIX)\ns.bind(os.environ['OP_SOCK'])\ns.listen(128)\nwhile True:\n c,_=s.accept()\n c.close()\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn identity(executable: PathBuf) -> Identity {
    Identity {
        connection: "first".into(),
        revision: "1".into(),
        package: "package".into(),
        program: "op".into(),
        executable,
        environment: "digest".into(),
    }
}

fn config() -> ProgramCache {
    ProgramCache {
        daemon_args: vec!["daemon".into(), "--timeout".into(), "0".into()],
        socket_env: "OP_SOCK".into(),
    }
}

#[tokio::test]
async fn replacing_an_identity_cancels_its_leases_and_stops_its_daemon() {
    let root = TempDir::new().unwrap();
    let caches = Caches::default();
    let original = identity(executable(&root));
    let lease = caches
        .acquire(original.clone(), &config(), true)
        .await
        .unwrap();
    let home = lease.home().to_owned();
    let mut changed = original;
    changed.environment = "rotated digest".into();
    let replacement = caches.acquire(changed, &config(), true).await.unwrap();
    assert!(lease.stop().is_cancelled());
    tokio::time::timeout(Duration::from_secs(2), async {
        while lease.entry.daemon.get().unwrap().alive() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_ne!(home, replacement.home());
    drop(lease);
    assert!(!home.exists());
    let home = replacement.home().to_owned();
    caches.clear();
    assert!(replacement.stop().is_cancelled());
    drop(replacement);
    assert!(!home.exists());
}

#[tokio::test]
async fn idle_invalid_and_ephemeral_caches_are_removed() {
    let root = TempDir::new().unwrap();
    let caches = Caches::default();
    let identity = identity(executable(&root));
    let lease = caches
        .acquire(identity.clone(), &config(), true)
        .await
        .unwrap();
    let home = lease.home().to_owned();
    // An active reader cannot expire while it is still using the daemon.
    *lease.entry.used.lock().unwrap() = Instant::now() - IDLE;
    caches.reconcile(|_| true);
    assert!(!lease.stop().is_cancelled());
    drop(lease);
    *caches
        .entries
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .used
        .lock()
        .unwrap() = Instant::now() - IDLE;
    caches.reconcile(|_| true);
    assert!(!home.exists());
    let lease = caches
        .acquire(identity.clone(), &config(), true)
        .await
        .unwrap();
    caches.reconcile(|_| false);
    assert!(lease.stop().is_cancelled());
    drop(lease);
    let lease = caches.acquire(identity, &config(), false).await.unwrap();
    let home = lease.home().to_owned();
    assert!(caches.entries.lock().unwrap().is_empty());
    drop(lease);
    assert!(!home.exists());
}

#[tokio::test]
async fn capacity_never_detaches_an_active_cache_from_invalidation() {
    let root = TempDir::new().unwrap();
    let caches = Caches::default();
    let original = identity(executable(&root));
    let lease = caches
        .acquire(original.clone(), &config(), true)
        .await
        .unwrap();
    let mut held = Vec::new();
    {
        let mut entries = caches.entries.lock().unwrap();
        for index in 1..RESIDENT {
            let mut identity = original.clone();
            identity.connection = format!("connection-{index}");
            let entry = Arc::new(Entry {
                identity: identity.clone(),
                daemon: OnceCell::new(),
                used: Mutex::new(Instant::now()),
                stop: CancellationToken::new(),
            });
            entries.insert((identity.connection, identity.program), entry.clone());
            held.push(entry);
        }
    }
    let mut extra = original;
    extra.connection = "extra".into();
    assert!(
        caches
            .acquire(extra.clone(), &config(), true)
            .await
            .is_err()
    );
    assert!(!lease.stop().is_cancelled());
    held.pop();
    let extra = caches.acquire(extra, &config(), true).await.unwrap();
    assert!(!lease.stop().is_cancelled());
    assert_eq!(caches.entries.lock().unwrap().len(), RESIDENT);
    caches.forget("first");
    assert!(lease.stop().is_cancelled());
    drop(extra);
}

#[tokio::test]
async fn failed_startup_can_be_retried_and_shutdown_removes_the_home() {
    let root = TempDir::new().unwrap();
    let path = executable(&root);
    let caches = Caches::default();
    let identity = identity(path.clone());
    std::fs::write(&path, "#!/bin/sh\nexit 1\n").unwrap();
    assert!(
        caches
            .acquire(identity.clone(), &config(), true)
            .await
            .is_err()
    );
    executable(&root);
    let lease = caches.acquire(identity, &config(), true).await.unwrap();
    let home = lease.home().to_owned();
    drop(lease);
    drop(caches);
    assert!(!home.exists());
}

#[tokio::test]
#[ignore = "requires CAIRN_TEST_OP pointing to the official CLI; no credentials or network"]
async fn real_cli_daemons_use_separate_sockets_and_are_reaped() {
    let executable = PathBuf::from(std::env::var("CAIRN_TEST_OP").unwrap());
    let caches = Caches::default();
    let original = identity(executable);
    let started = Instant::now();
    let first = caches
        .acquire(original.clone(), &config(), true)
        .await
        .unwrap();
    let cold = started.elapsed();
    let started = Instant::now();
    let warm = caches
        .acquire(original.clone(), &config(), true)
        .await
        .unwrap();
    let warm_time = started.elapsed();
    assert_eq!(first.socket(), warm.socket());
    let mut other = original;
    other.connection = "other".into();
    let second = caches.acquire(other, &config(), true).await.unwrap();
    assert_ne!(first.socket(), second.socket());
    eprintln!("Official CLI foreground startup {cold:?}; warm lease {warm_time:?}");
    let home = first.home().to_owned();
    caches.clear();
    assert!(first.stop().is_cancelled());
    drop(first);
    drop(warm);
    drop(second);
    assert!(!home.exists());
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires CAIRN_TEST_OP pointing to the official CLI; no credentials or network"]
async fn real_cli_background_replacement_is_also_reaped() {
    let executable = PathBuf::from(std::env::var("CAIRN_TEST_OP").unwrap());
    let caches = Caches::default();
    let lease = caches
        .acquire(identity(executable.clone()), &config(), true)
        .await
        .unwrap();
    let daemon = lease.entry.daemon.get().unwrap();
    daemon.child.lock().unwrap().start_kill().unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while daemon.alive() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // Reproduce the vendor client's automatic replacement using the same official
    // executable and private socket, without issuing an authenticated request.
    let status = tokio::process::Command::new(executable)
        .args(["daemon", "--background", "--timeout", "0"])
        .env_clear()
        .env("HOME", lease.home())
        .env("TMPDIR", lease.home())
        .env("OP_SOCK", lease.socket())
        .current_dir(lease.home())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .unwrap();
    assert!(status.success());
    let pid = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(stream) = tokio::net::UnixStream::connect(lease.socket()).await {
                break stream.peer_cred().unwrap().pid().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    caches.clear();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
            if stat.is_err() || stat.is_ok_and(|s| s.split_once(") ").unwrap().1.starts_with('Z')) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
