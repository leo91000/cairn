//! Resolve Docker references before forwarding so name changes cannot redirect
//! a request after its saved HostConfig has been authorized.
use crate::error::{Error, Result};
use axum::http::{Method, Uri};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn identifier(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn resolve(store: &Path, name: &str) -> Result<PathBuf> {
    let mut exact = None;
    let mut named = None;
    let mut prefixes = Vec::new();
    for entry in std::fs::read_dir(store.join("containers"))? {
        let entry = entry?;
        let id = entry.file_name();
        let id = id.to_string_lossy();
        if !identifier(&id) {
            continue;
        }
        if id == name {
            exact = Some(entry.path());
        }
        if id.starts_with(name) {
            prefixes.push(entry.path());
        }
        let bytes = match std::fs::read(entry.path().join("config.v2.json")) {
            Ok(bytes) => bytes,
            // Docker creates the directory before saving its config, and can
            // remove it after read_dir. Missing unrelated metadata is ordinary.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let config: Value = serde_json::from_slice(&bytes)?;
        if config["Name"] == format!("/{name}") && named.replace(entry.path()).is_some() {
            return Err(Error::forbidden("Ambiguous Docker container name."));
        }
    }
    if let Some(exact) = exact {
        // Docker falls back to names if an ID is concurrently removed. An
        // alias equal to a full ID could otherwise switch the authorized target.
        if named.as_ref().is_some_and(|named| *named != exact) {
            return Err(Error::forbidden(
                "Docker ID/name collisions require removing or renaming the alias.",
            ));
        }
        return Ok(exact);
    }
    if let Some(named) = named {
        return Ok(named);
    }
    if prefixes.len() == 1 {
        return Ok(prefixes.pop().unwrap());
    }
    Err(Error::forbidden(if prefixes.is_empty() {
        "Docker container is unavailable."
    } else {
        "Ambiguous Docker container ID."
    }))
}

pub(super) fn pin_uri(uri: &mut Uri, method: &Method, store: &Path) -> Result<()> {
    let original = uri.path();
    let path = super::api_path(original)?;
    let Some(reference) = path.strip_prefix("/containers/") else {
        return Ok(());
    };
    let (name, suffix) = reference
        .split_once('/')
        .map_or((reference, ""), |(name, rest)| (name, rest));
    if reference == name && matches!(name, "create" | "json" | "prune")
        || method == Method::GET && suffix == "json"
        || method == Method::DELETE && suffix.is_empty()
    {
        return Ok(());
    }
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        || path.contains('%')
        || path.contains("..")
        || path.contains("//")
    {
        return Err(Error::forbidden("Invalid Docker container path."));
    }
    let directory = resolve(store, name)?;
    let id = directory
        .file_name()
        .and_then(|id| id.to_str())
        .ok_or_else(|| Error::forbidden("Invalid Docker container ID."))?;
    let offset = original.len() - path.len() + "/containers/".len();
    let query = uri
        .query()
        .map_or(String::new(), |query| format!("?{query}"));
    let pinned = format!(
        "{}{}{}{}",
        &original[..offset],
        id,
        &original[offset + name.len()..],
        query
    );
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(pinned.parse().map_err(Error::internal)?);
    *uri = Uri::from_parts(parts).map_err(Error::internal)?;
    Ok(())
}

pub(super) fn canonical(name: &str) -> bool {
    identifier(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde_json::json;
    use std::sync::Arc;

    fn container(store: &Path, id: &str, name: &str, privileged: bool) {
        let directory = store.join("containers").join(id);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("config.v2.json"),
            serde_json::to_vec(&json!({ "Name": format!("/{name}") })).unwrap(),
        )
        .unwrap();
        std::fs::write(
            directory.join("hostconfig.json"),
            serde_json::to_vec(&json!({ "Privileged": privileged })).unwrap(),
        )
        .unwrap();
    }

    fn authorize(store: &Path, uri: &Uri) -> Result<()> {
        super::super::Policy {
            plan: Arc::new(json!({ "sandbox": "workspace-write" })),
        }
        .authorize_in(
            &json!({
                "RequestUri": uri.to_string(),
                "RequestMethod": "POST",
                "RequestBody": STANDARD.encode(br#"{"Cmd":["true"]}"#),
            }),
            store,
        )
    }

    #[test]
    fn unrelated_missing_metadata_keeps_valid_operations_and_selected_policy_checks() {
        let root = tempfile::tempdir().unwrap();
        let id = "a".repeat(64);
        container(root.path(), &id, "fixture", false);
        // Both creation-before-checkpoint and deletion-after-enumeration can
        // present a directory with no persisted config to this external reader.
        std::fs::create_dir_all(root.path().join("containers").join("b".repeat(64))).unwrap();
        for reference in [&id, &id[..12], "fixture"] {
            for operation in ["start", "exec", "wait"] {
                let mut uri: Uri =
                    format!("/v1.56/containers/{reference}/{operation}?detach=false")
                        .parse()
                        .unwrap();
                pin_uri(&mut uri, &Method::POST, root.path()).unwrap();
                assert_eq!(
                    uri.to_string(),
                    format!("/v1.56/containers/{id}/{operation}?detach=false")
                );
                authorize(root.path(), &uri).unwrap();
            }
        }
        let uri: Uri = format!("/containers/{id}/start").parse().unwrap();
        std::fs::write(
            root.path()
                .join("containers")
                .join(&id)
                .join("hostconfig.json"),
            br#"{"Privileged":true}"#,
        )
        .unwrap();
        assert!(authorize(root.path(), &uri).is_err());
        std::fs::remove_file(
            root.path()
                .join("containers")
                .join(&id)
                .join("hostconfig.json"),
        )
        .unwrap();
        assert!(authorize(root.path(), &uri).is_err());
    }

    #[test]
    fn exact_names_precede_prefixes_and_pinned_requests_survive_name_changes() {
        let root = tempfile::tempdir().unwrap();
        let id = "a".repeat(64);
        let prefix_a = format!("db{}", "b".repeat(62));
        let prefix_b = format!("db{}", "c".repeat(62));
        container(root.path(), &id, "db", false);
        container(root.path(), &prefix_a, "other", true);
        container(root.path(), &prefix_b, "another", false);
        assert_eq!(
            resolve(root.path(), "db").unwrap(),
            root.path().join("containers").join(&id)
        );
        let mut pinned: Uri = "/containers/db/start".parse().unwrap();
        pin_uri(&mut pinned, &Method::POST, root.path()).unwrap();
        // After pinning, a name change must not make Docker's ID-prefix fallback
        // activate the unsafe saved configuration of a different container.
        container(root.path(), &id, "renamed", false);
        assert!(resolve(root.path(), "db").is_err());
        std::fs::remove_dir_all(root.path().join("containers").join(&prefix_b)).unwrap();
        assert_eq!(
            resolve(root.path(), "db").unwrap(),
            root.path().join("containers").join(&prefix_a)
        );
        authorize(root.path(), &pinned).unwrap();
        assert!(authorize(root.path(), &"/containers/db/start".parse().unwrap()).is_err());
        // An alias equal to a full ID can make Docker fall back even after ID
        // pinning. Refuse the collision instead of authorizing a different target.
        container(root.path(), &prefix_a, &id, true);
        assert!(authorize(root.path(), &pinned).is_err());
    }

    #[test]
    fn collection_route_names_cannot_bypass_saved_container_checks() {
        let root = tempfile::tempdir().unwrap();
        for (index, name) in ["create", "json", "prune"].into_iter().enumerate() {
            let id = format!("{index:064x}");
            container(root.path(), &id, name, true);
            let mut uri: Uri = format!("/containers/{name}/start").parse().unwrap();
            pin_uri(&mut uri, &Method::POST, root.path()).unwrap();
            assert_eq!(uri.to_string(), format!("/containers/{id}/start"));
            assert!(authorize(root.path(), &uri).is_err());
            assert!(
                authorize(
                    root.path(),
                    &format!("/containers/{name}/start").parse().unwrap()
                )
                .is_err()
            );
        }
        // Collection operations still reach their ordinary policy handlers.
        for path in [
            "/containers/create",
            "/containers/json",
            "/containers/prune",
        ] {
            let mut uri: Uri = path.parse().unwrap();
            pin_uri(&mut uri, &Method::POST, root.path()).unwrap();
            assert_eq!(uri.to_string(), path);
        }
    }
}
