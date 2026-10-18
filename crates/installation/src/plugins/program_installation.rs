//! Package-scoped program preparation. Luau chooses releases; Cairn owns binary
//! I/O, immutable executable files and the atomic installed/previous release lock.
use super::{
    capabilities,
    registry::{self, Package},
};
use crate::{
    error::{Error, Result},
    service::Service,
    validation::text,
};
use cairn_plugin_api::{Manifest, Program, Sources};
use cairn_plugin_host::Denied;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncWriteExt;

const MAX_DOWNLOAD: u64 = 64 * 1024 * 1024;
const MAX_EXECUTABLE: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Release {
    version: String,
    url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    format: String,
    executable: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Installed {
    release: Release,
    archive_sha256: String,
    binary_sha256: String,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Lock {
    installed: Option<Installed>,
    previous: Option<Installed>,
    candidate: Option<Release>,
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 240
        && !path.contains(['\\', '\0', ':'])
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

fn validate(s: &Service, manifest: &Manifest, release: &Release) -> Result<()> {
    if release.version.is_empty()
        || release.version.len() > 100
        || release.version.chars().any(char::is_control)
        || release.url.len() > 2048
        || !relative(&release.executable)
        || !matches!(release.format.as_str(), "zip" | "binary")
        || release.sha256.as_ref().is_some_and(|value| !hash(value))
    {
        return Err(Error::bad("The installer returned an invalid release."));
    }
    capabilities::target(s, manifest, &release.url, "https")?;
    Ok(())
}

fn platform() -> Value {
    json!({ "os": std::env::consts::OS, "arch": std::env::consts::ARCH })
}

fn root(s: &Service, package: &Package, program: &Program) -> PathBuf {
    s.config
        .data_dir
        .join("plugin-programs")
        .join(&package.manifest.id)
        .join(&package.digest)
        .join(&program.name)
        .join(format!(
            "{}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
}

fn read_lock(directory: &Path) -> Result<Lock> {
    let path = directory.join("lock.json");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Lock::default()),
        Ok(metadata) if metadata.is_file() && metadata.len() <= 32_768 => {
            serde_json::from_slice(&std::fs::read(path)?)
                .map_err(|_| Error::unavailable("The installed program lock is invalid."))
        }
        _ => Err(Error::unavailable(
            "The installed program lock is unavailable.",
        )),
    }
}

fn write_lock(directory: &Path, lock: &Lock) -> Result<()> {
    std::fs::create_dir_all(directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&serde_json::to_vec_pretty(lock)?)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(directory.join("lock.json"))
        .map_err(|error| error.error)?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

fn installed_path(directory: &Path, installed: &Installed) -> Result<PathBuf> {
    if !hash(&installed.binary_sha256) || !hash(&installed.archive_sha256) {
        return Err(Error::unavailable("The installed program lock is invalid."));
    }
    let path = directory.join(&installed.binary_sha256);
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_file()
        || metadata.len() > MAX_EXECUTABLE
        || sha256(&std::fs::read(&path)?) != installed.binary_sha256
    {
        return Err(Error::unavailable(
            "The installed program failed its integrity check.",
        ));
    }
    Ok(path)
}

fn candidate_digest(release: &Release) -> Result<String> {
    Ok(sha256(&serde_json::to_vec(release)?))
}

fn summary(program: &Program, lock: &Lock) -> Result<Value> {
    Ok(json!({
        "name": program.name,
        "installer": program.installer,
        "installed": lock.installed,
        "previous": lock.previous,
        "candidate": lock.candidate,
        "digest": lock.candidate.as_ref().map(candidate_digest).transpose()?,
    }))
}

/// Only the installer module is loaded. main.luau and instance handlers are not run.
pub(crate) fn source(package: &Package, program: &Program) -> Sources {
    let module = program.installer.trim_end_matches(".luau");
    Sources {
        main: format!(
            "local installer = require(\"./{module}\")\nassert(type(installer.resolve) == 'function' and type(installer.install) == 'function', 'Installer must export resolve and install')\nreturn installer"
        ),
        modules: package.source.modules.clone(),
    }
}

async fn metadata(s: &Service, manifest: &Manifest, args: &Value) -> Result<Value> {
    let authentication_requested = !args["auth"].is_null() && args["auth"] != false;
    if authentication_requested
        || args["method"]
            .as_str()
            .is_some_and(|method| method != "GET")
    {
        return Err(Error::forbidden(
            "Installers can only fetch public release metadata.",
        ));
    }
    let url = capabilities::target(s, manifest, text(args, "url"), "https")?;
    let response = s
        .plugins
        .http
        .get(url)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| Error::bad_gateway("Release metadata is unavailable."))?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Error::bad_gateway("Release metadata was interrupted."))?;
        if bytes.len() + chunk.len() > 1_048_576 {
            return Err(Error::too_large("Release metadata is larger than 1 MB."));
        }
        bytes.extend_from_slice(&chunk);
    }
    let parsed = serde_json::from_slice::<Value>(&bytes).ok();
    let response = json!({
        "status": status,
        "json": parsed,
        "text": if parsed.is_none() { json!(String::from_utf8_lossy(&bytes)) } else { Value::Null },
    });
    if serde_json::to_vec(&response)?.len() > cairn_plugin_api::MAX_FRAME_BYTES - 1024 {
        return Err(Error::too_large(
            "Release metadata does not fit in a worker message.",
        ));
    }
    Ok(response)
}

fn invocation_error(message: String, refused: Option<Error>) -> Error {
    if let Some(message) = message.strip_prefix(cairn_plugin_api::REJECTED) {
        Error::bad_gateway(message)
    } else {
        refused.unwrap_or_else(|| Error::bad_gateway(message))
    }
}

async fn resolve(s: &Service, package: &Package, program: &Program) -> Result<Release> {
    let refused = std::sync::Mutex::new(None);
    let refused_ref = &refused;
    let value = s
        .plugins
        .installer_host
        .invoke(
            &s.plugins.executable()?,
            &package.manifest,
            source(package, program),
            "resolve",
            json!({ "platform": platform() }),
            |method, args| async move {
                if method != "http.request" {
                    return Err(Denied("forbidden"));
                }
                metadata(s, &package.manifest, &args)
                    .await
                    .map_err(|error| {
                        let code = capabilities::code(&error);
                        *refused_ref.lock().unwrap() = Some(error);
                        Denied(code)
                    })
            },
        )
        .await
        .map_err(|error| invocation_error(error, refused.into_inner().unwrap()))?;
    let release: Release = serde_json::from_value(value)
        .map_err(|_| Error::bad("The installer must resolve an exact release."))?;
    validate(s, &package.manifest, &release)?;
    Ok(release)
}

struct Preparation {
    directory: tempfile::TempDir,
    archive_sha256: Option<String>,
    extracted: bool,
}

impl Preparation {
    async fn download(
        &mut self,
        s: &Service,
        manifest: &Manifest,
        release: &Release,
        args: &Value,
    ) -> Result<Value> {
        if self.archive_sha256.is_some()
            || text(args, "url") != release.url
            || args["sha256"].as_str() != release.sha256.as_deref()
        {
            return Err(Error::bad(
                "Download the exact release selected by resolve.",
            ));
        }
        let url = capabilities::target(s, manifest, &release.url, "https")?;
        let response = s
            .plugins
            .http
            .get(url)
            .timeout(Duration::from_secs(90))
            .send()
            .await
            .map_err(|_| Error::bad_gateway("The program download failed."))?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|size| size > MAX_DOWNLOAD)
        {
            return Err(Error::bad_gateway(
                "The program archive is unavailable or too large.",
            ));
        }
        let mut output = tokio::fs::File::create(self.directory.path().join("archive")).await?;
        let mut stream = response.bytes_stream();
        let mut size = 0;
        let mut digest = Sha256::new();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|_| Error::bad_gateway("The program download was interrupted."))?;
            size += chunk.len() as u64;
            if size > MAX_DOWNLOAD {
                return Err(Error::too_large("The program archive exceeds 64 MB."));
            }
            digest.update(&chunk);
            output.write_all(&chunk).await?;
        }
        output.sync_all().await?;
        let digest = hex::encode(digest.finalize());
        if release
            .sha256
            .as_ref()
            .is_some_and(|expected| *expected != digest)
        {
            return Err(Error::bad_gateway(
                "The program archive failed its SHA-256 check.",
            ));
        }
        self.archive_sha256 = Some(digest.clone());
        Ok(json!({ "id": "archive", "sha256": digest }))
    }

    fn extract(&mut self, release: &Release, args: &Value) -> Result<Value> {
        if self.archive_sha256.is_none()
            || self.extracted
            || text(args, "archive") != "archive"
            || text(args, "path") != release.executable
            || text(args, "format") != release.format
        {
            return Err(Error::bad("Extract the executable declared by resolve."));
        }
        let archive = std::fs::File::open(self.directory.path().join("archive"))?;
        let mut input: Box<dyn Read> = if release.format == "zip" {
            let mut zip = zip::ZipArchive::new(archive)
                .map_err(|_| Error::bad("Invalid ZIP program archive."))?;
            if zip.len() > 256 {
                return Err(Error::too_large(
                    "The program archive has too many entries.",
                ));
            }
            for index in 0..zip.len() {
                let entry = zip
                    .by_index(index)
                    .map_err(|_| Error::bad("Invalid ZIP entry."))?;
                let name = entry.name().trim_end_matches('/');
                let kind = entry.unix_mode().unwrap_or(0) & 0o170000;
                if !relative(name) || !matches!(kind, 0 | 0o100000 | 0o040000) {
                    return Err(Error::bad(
                        "Program archives cannot contain links or unsafe paths.",
                    ));
                }
            }
            let mut entry = zip
                .by_name(&release.executable)
                .map_err(|_| Error::bad("The executable is missing from the archive."))?;
            if entry.is_dir() || entry.size() > MAX_EXECUTABLE {
                return Err(Error::too_large("The executable is invalid or too large."));
            }
            // Only the selected regular file is read; archive paths never become disk paths.
            let mut bytes = Vec::new();
            entry
                .by_ref()
                .take(MAX_EXECUTABLE + 1)
                .read_to_end(&mut bytes)?;
            Box::new(std::io::Cursor::new(bytes))
        } else {
            Box::new(archive)
        };
        let mut output = std::fs::File::create(self.directory.path().join("executable"))?;
        let size = std::io::copy(&mut input.by_ref().take(MAX_EXECUTABLE + 1), &mut output)?;
        if size == 0 || size > MAX_EXECUTABLE {
            return Err(Error::too_large("The executable is empty or too large."));
        }
        output.set_permissions(std::fs::Permissions::from_mode(0o555))?;
        output.sync_all()?;
        self.extracted = true;
        Ok(json!({ "id": "executable" }))
    }
}

async fn install(
    s: &Service,
    package: &Package,
    program: &Program,
    directory: &Path,
    lock: &mut Lock,
    release: Release,
) -> Result<()> {
    validate(s, &package.manifest, &release)?;
    if let Some(installed) = &lock.installed
        && installed.release == release
    {
        installed_path(directory, installed)?;
        lock.candidate = None;
        return write_lock(directory, lock);
    }
    std::fs::create_dir_all(directory)?;
    let preparation = tokio::sync::Mutex::new(Preparation {
        directory: tempfile::tempdir_in(directory)?,
        archive_sha256: None,
        extracted: false,
    });
    let refused = std::sync::Mutex::new(None);
    let refused_ref = &refused;
    let result = s
        .plugins
        .installer_host
        .invoke(
            &s.plugins.executable()?,
            &package.manifest,
            source(package, program),
            "install",
            json!({ "platform": platform(), "release": release }),
            |method, args| {
                let preparation = &preparation;
                let release = &release;
                async move {
                    let mut preparation = preparation.lock().await;
                    let result = match method.as_str() {
                        "download" => {
                            preparation
                                .download(s, &package.manifest, release, &args)
                                .await
                        }
                        "archive.executable" => preparation.extract(release, &args),
                        _ => Err(Error::forbidden(
                            "Only program preparation is allowed during install.",
                        )),
                    };
                    result.map_err(|error| {
                        let code = capabilities::code(&error);
                        *refused_ref.lock().unwrap() = Some(error);
                        Denied(code)
                    })
                }
            },
        )
        .await
        .map_err(|error| invocation_error(error, refused.into_inner().unwrap()))?;
    let preparation = preparation.into_inner();
    if result != json!({ "id": "executable" }) || !preparation.extracted {
        return Err(Error::bad(
            "The installer must return its prepared executable.",
        ));
    }
    let binary = preparation.directory.path().join("executable");
    let binary_sha256 = sha256(&std::fs::read(&binary)?);
    let installed = Installed {
        release,
        archive_sha256: preparation.archive_sha256.expect("extracted archive"),
        binary_sha256,
    };
    let target = directory.join(&installed.binary_sha256);
    std::fs::rename(binary, target)?;
    lock.previous = lock.installed.take();
    lock.installed = Some(installed);
    lock.candidate = None;
    write_lock(directory, lock)
}

/// First use of a bundled program installs it once. Explicit paths and test doubles
/// keep their existing behavior; later calls always reuse the locked release.
pub async fn executable(s: &Service, manifest: &Manifest, program: &Program) -> Result<PathBuf> {
    if let Some(path) = s.plugins.program_override(&manifest.id, program) {
        return Ok(path);
    }
    if program.installer.is_empty() {
        return Ok(PathBuf::from(&program.path));
    }
    let package = registry::active(s, &manifest.id).await?;
    let same_manifest = serde_json::to_value(&package.manifest)? == serde_json::to_value(manifest)?;
    if !same_manifest {
        return Err(Error::conflict("The plugin changed. Retry the command."));
    }
    let directory = root(s, &package, program);
    // An update must not block uses of the already installed executable.
    if let Some(installed) = read_lock(&directory)?.installed {
        return installed_path(&directory, &installed);
    }

    let _guard = s.plugins.program_installation.lock().await;
    let mut lock = read_lock(&directory)?;
    if lock.installed.is_none() {
        let release = resolve(s, &package, program).await?;
        install(s, &package, program, &directory, &mut lock, release).await?;
    }
    installed_path(
        &directory,
        lock.installed.as_ref().expect("installed program"),
    )
}

/// Cold calls need enough wall time for preparation as well as the normal command.
/// This does not eagerly install dependencies for metadata-only handlers.
pub(crate) fn needs_preparation(s: &Service, package: &Package) -> bool {
    package.manifest.programs.iter().any(|program| {
        !program.installer.is_empty()
            && s.plugins
                .program_override(&package.manifest.id, program)
                .is_none()
            && read_lock(&root(s, package, program)).is_ok_and(|lock| lock.installed.is_none())
    })
}

/// Prepare before activating an uploaded version; failure leaves the old version active.
pub async fn prepare(s: &Service, package: &Package) -> Result<()> {
    if !package
        .manifest
        .programs
        .iter()
        .any(|program| !program.installer.is_empty())
    {
        return Ok(());
    }
    let _guard = s.plugins.program_installation.lock().await;
    for program in package
        .manifest
        .programs
        .iter()
        .filter(|program| !program.installer.is_empty())
    {
        let directory = root(s, package, program);
        let mut lock = read_lock(&directory)?;
        if let Some(installed) = &lock.installed {
            installed_path(&directory, installed)?;
        } else {
            let release = resolve(s, package, program).await?;
            install(s, package, program, &directory, &mut lock, release).await?;
        }
    }
    Ok(())
}

pub async fn list(s: &Service, id: &str) -> Result<Value> {
    let package = registry::active(s, id).await?;
    let mut programs = Vec::new();
    for program in package
        .manifest
        .programs
        .iter()
        .filter(|program| !program.installer.is_empty())
    {
        programs.push(summary(program, &read_lock(&root(s, &package, program))?)?);
    }
    Ok(json!({ "programs": programs }))
}

pub async fn action(
    s: &Service,
    id: &str,
    name: &str,
    action: &str,
    input: &Value,
) -> Result<Value> {
    let package = registry::active(s, id).await?;
    let program = package
        .manifest
        .program(name)
        .filter(|program| !program.installer.is_empty())
        .ok_or_else(|| Error::not_found("This program has no installer."))?;
    let _guard = s.plugins.program_installation.lock().await;
    let directory = root(s, &package, program);
    let mut lock = read_lock(&directory)?;
    match action {
        "check" => {
            lock.candidate = Some(resolve(s, &package, program).await?);
            write_lock(&directory, &lock)?;
        }
        "install" => {
            let release = lock
                .candidate
                .clone()
                .ok_or_else(|| Error::conflict("Check for a release before installing it."))?;
            if text(input, "digest") != candidate_digest(&release)? {
                return Err(Error::conflict(
                    "The candidate release changed. Check for updates again.",
                ));
            }
            install(s, &package, program, &directory, &mut lock, release).await?;
        }
        "rollback" => {
            let previous = lock
                .previous
                .take()
                .ok_or_else(|| Error::conflict("There is no previous program release."))?;
            installed_path(&directory, &previous)?;
            lock.previous = lock.installed.take();
            lock.installed = Some(previous);
            lock.candidate = None;
            write_lock(&directory, &lock)?;
        }
        _ => return Err(Error::not_found("Unknown program action.")),
    }
    if registry::active(s, id).await?.digest != package.digest {
        return Err(Error::conflict(
            "The plugin changed during program preparation.",
        ));
    }
    let audit = json!({
        "plugin": id,
        "program": name,
        "packageDigest": package.digest,
        "version": lock.installed.as_ref().map(|installed| &installed.release.version),
    });
    let event = format!("plugin.program.{action}");
    s.store
        .transaction(move |db| db.audit(&event, &audit))
        .await?;
    summary(program, &lock)
}
