//! Schema-only templates from the exact guest Codex build. Existing state is
//! never replaced; interrupted installations safely fall back to native migration.
use serde::Deserialize;
use std::{
    fs::{self, File},
    io::{self, Write},
    path::Path,
};

#[derive(Default)]
pub(super) struct Usage {
    pub state: u64,
    pub logs: u64,
    pub history: u64,
    pub other: u64,
    pub complete: bool,
}

/// Bounded metadata only: no directory traversal, symlink following or contents.
pub(super) fn usage(home: &Path) -> Usage {
    let mut usage = Usage::default();
    if !fs::symlink_metadata(home).is_ok_and(|meta| meta.is_dir()) {
        return usage;
    }
    let Ok(entries) = fs::read_dir(home) else {
        return usage;
    };
    usage.complete = true;
    for (index, entry) in entries.take(129).enumerate() {
        if index == 128 {
            usage.complete = false;
            break;
        }
        let Ok(entry) = entry else {
            usage.complete = false;
            continue;
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".sqlite") && !name.ends_with(".sqlite-wal") {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            usage.complete = false;
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let bytes = if name.starts_with("state_") {
            &mut usage.state
        } else if name.starts_with("logs_") {
            &mut usage.logs
        } else if name.starts_with("thread_history_") {
            &mut usage.history
        } else {
            &mut usage.other
        };
        *bytes += metadata.len();
    }
    usage
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    files: Vec<String>,
}

pub(super) fn install(home: &Path, templates: &Path, uid: u32, gid: u32) -> io::Result<usize> {
    if !templates.exists() || !fs::symlink_metadata(home).is_ok_and(|meta| meta.is_dir()) {
        return Ok(0);
    }
    for entry in fs::read_dir(home)? {
        if entry?.file_name().to_string_lossy().contains(".sqlite") {
            return Ok(0);
        }
    }
    let manifest: Manifest = serde_json::from_slice(&fs::read(templates.join("manifest.json"))?)?;
    if manifest.version != 1 || manifest.files.is_empty() || manifest.files.len() > 16 {
        return Err(io::Error::other("Invalid Codex schema template manifest"));
    }
    let mut databases = Vec::new();
    let mut bytes = 0;
    for name in manifest.files {
        let valid_name = name.strip_suffix(".sqlite").is_some_and(|stem| {
            !stem.is_empty()
                && stem
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        });
        let source = templates.join(&name);
        if !valid_name {
            return Err(io::Error::other("Invalid Codex schema template"));
        }
        let metadata = fs::symlink_metadata(&source)?;
        if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
            return Err(io::Error::other("Invalid Codex schema template"));
        }
        let data = fs::read(source)?;
        bytes += data.len();
        if data.len() > 4 * 1024 * 1024 || bytes > 16 * 1024 * 1024 {
            return Err(io::Error::other(
                "Codex schema templates exceed their budget",
            ));
        }
        databases.push((name, data));
    }
    for (name, data) in &databases {
        let mut temporary = tempfile::Builder::new()
            .prefix(".cairn-codex-seed-")
            .tempfile_in(home)?;
        temporary.write_all(data)?;
        std::os::unix::fs::chown(temporary.path(), Some(uid), Some(gid))?;
        temporary.as_file().sync_all()?;
        temporary.persist_noclobber(home.join(name))?;
        File::open(home)?.sync_all()?;
    }
    Ok(databases.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    fn fixture(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let home = root.join("home");
        let templates = root.join("templates");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&templates).unwrap();
        fs::write(
            templates.join("manifest.json"),
            json!({ "version": 1, "files": ["state_5.sqlite", "logs_2.sqlite"] }).to_string(),
        )
        .unwrap();
        for name in ["state_5.sqlite", "logs_2.sqlite"] {
            let db = rusqlite::Connection::open(templates.join(name)).unwrap();
            db.execute_batch("CREATE TABLE schema_fixture(id INTEGER PRIMARY KEY);")
                .unwrap();
        }
        (home, templates)
    }

    #[test]
    fn new_homes_receive_valid_private_templates_without_replacing_existing_state() {
        let root = tempfile::tempdir().unwrap();
        let (home, templates) = fixture(root.path());
        let uid = unsafe { libc::geteuid() };
        assert_eq!(
            install(&home, &templates, uid, unsafe { libc::getegid() }).unwrap(),
            2
        );
        let db = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
        db.execute("INSERT INTO schema_fixture VALUES (37)", [])
            .unwrap();
        assert_eq!(
            install(&home, &templates, uid, unsafe { libc::getegid() }).unwrap(),
            0
        );
        let value: i64 = db
            .query_row("SELECT id FROM schema_fixture", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, 37);
        assert_eq!(
            fs::metadata(home.join("state_5.sqlite"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn interrupted_or_user_supplied_state_and_symlinks_are_preserved() {
        for name in ["state_5.sqlite-wal", "state_5.sqlite", "custom.sqlite"] {
            let root = tempfile::tempdir().unwrap();
            let (home, templates) = fixture(root.path());
            fs::write(home.join(name), b"existing").unwrap();
            assert_eq!(
                install(&home, &templates, unsafe { libc::geteuid() }, unsafe {
                    libc::getegid()
                })
                .unwrap(),
                0
            );
            assert_eq!(fs::read(home.join(name)).unwrap(), b"existing");
            assert!(!home.join("logs_2.sqlite").exists());
        }
        let root = tempfile::tempdir().unwrap();
        let (home, templates) = fixture(root.path());
        let alias = root.path().join("home-alias");
        std::os::unix::fs::symlink(home, &alias).unwrap();
        assert_eq!(
            install(&alias, &templates, unsafe { libc::geteuid() }, unsafe {
                libc::getegid()
            })
            .unwrap(),
            0
        );
    }

    #[test]
    fn invalid_templates_are_rejected_before_installation() {
        let root = tempfile::tempdir().unwrap();
        let (home, templates) = fixture(root.path());
        fs::write(
            templates.join("manifest.json"),
            json!({ "version": 1, "files": ["state_5.sqlite", "../escape.sqlite"] }).to_string(),
        )
        .unwrap();
        assert!(
            install(&home, &templates, unsafe { libc::geteuid() }, unsafe {
                libc::getegid()
            })
            .is_err()
        );
        assert_eq!(fs::read_dir(home).unwrap().count(), 0);
    }

    #[test]
    fn metadata_counts_database_and_wal_sizes_without_credentials_or_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        for (name, length) in [
            ("state_5.sqlite", 12),
            ("state_5.sqlite-wal", 13),
            ("logs_2.sqlite", 37),
            ("thread_history_1.sqlite-wal", 11),
            ("goals_1.sqlite", 5),
            ("auth.json", 100),
        ] {
            fs::write(home.join(name), vec![0; length]).unwrap();
        }
        std::os::unix::fs::symlink(home.join("auth.json"), home.join("state_link.sqlite")).unwrap();
        let measured = usage(home);
        assert!(measured.complete);
        assert_eq!(measured.state, 25);
        assert_eq!(measured.logs, 37);
        assert_eq!(measured.history, 11);
        assert_eq!(measured.other, 5);
    }
}
