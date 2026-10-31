pub mod bundle;
pub mod diff;
pub(crate) mod lifecycle;
pub mod sources;
mod usage;
pub mod zip;

use crate::{
    config::Config,
    error::{Error, Result},
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use bundle::{Base, Change};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::LazyLock,
};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};

static NAME: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[a-z0-9][a-z0-9-]{0,63}$").unwrap());

pub fn name(value: &str) -> Result<()> {
    if !NAME.is_match(value) {
        return Err(Error::bad(
            "Use a skill name with lowercase letters, numbers, and hyphens.",
        ));
    }
    Ok(())
}

static CODE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?s)```.*?```|`[^`\n]*`").unwrap());

static MENTION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?:^|[^A-Za-z0-9_$\\])\$([a-z0-9][a-z0-9-]{0,63})").unwrap()
});

/// Skill names invoked as `$name` outside code, in first-mention order.
/// The web and Android composers highlight the same tokens.
pub fn mentions(text: &str, names: &[&str]) -> Vec<String> {
    let text = CODE.replace_all(text, " ");
    let mut found = Vec::new();
    for capture in MENTION.captures_iter(&text) {
        let token = capture.get(1).unwrap();
        let name = token.as_str();
        let next = text[token.end()..].chars().next();
        if next.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            || !names.contains(&name)
            || found.iter().any(|f| f == name)
        {
            continue;
        }
        found.push(name.to_owned());
    }
    found
}

pub(crate) use cairn_contracts::SkillMetadata as Metadata;

pub(crate) fn metadata(content: &str) -> Result<Metadata> {
    if content.len() > 100000 {
        return Err(Error::bad("Skill is too large (maximum 100 KB)."));
    }
    let missing = || Error::bad("SKILL.md needs YAML frontmatter with name and description.");
    let normalized = content.replace("\r\n", "\n");
    let body = normalized.strip_prefix("---\n").ok_or_else(missing)?;
    let end = body
        .match_indices("\n---")
        .find(|(index, _)| {
            let rest = &body[index + 4..];
            rest.is_empty() || rest.starts_with('\n')
        })
        .map(|(index, _)| index)
        .ok_or_else(missing)?;
    let data: Value = serde_yaml_ng::from_str(&body[..end])
        .map_err(|_| Error::bad("Invalid YAML frontmatter."))?;
    let description = text(&data, "description");
    if !data["name"].is_string() || description.trim().is_empty() {
        return Err(Error::bad(
            "Add a name and description to the skill frontmatter.",
        ));
    }
    let name_value = text(&data, "name");
    name(name_value)?;
    Ok(Metadata {
        name: name_value.to_owned(),
        description: description.to_owned(),
    })
}

pub fn parse(content: &str) -> Result<Value> {
    Ok(serde_json::to_value(metadata(content)?)?)
}

fn read_listing(
    listing: &mut cairn_contracts::Skill,
    directory: &str,
    path: &Path,
    content: String,
) {
    listing.path = path.to_string_lossy().into_owned();
    match metadata(&content) {
        Ok(parsed) => {
            listing.valid = parsed.name == directory;
            if !listing.valid {
                listing.error = Some("Name differs from directory".into());
            }
            listing.name = parsed.name;
            listing.description = parsed.description;
        }
        Err(error) => listing.error = Some(error.message),
    }
    listing.content = content;
}

pub fn relative(value: &str) -> Result<()> {
    if Path::new(value).is_absolute()
        || value.contains('\0')
        || value
            .split(['/', '\\'])
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(Error::bad("Invalid file path."));
    }
    Ok(())
}

pub async fn bounded(root: &Path, relative_path: &str, missing: bool) -> Result<PathBuf> {
    relative(relative_path)?;
    let base = fs::canonicalize(root).await?;
    let path = base.join(relative_path);
    let resolved = match fs::canonicalize(&path).await {
        Ok(path) => path,
        Err(error) if missing && error.kind() == std::io::ErrorKind::NotFound => {
            fs::canonicalize(path.parent().unwrap())
                .await?
                .join(path.file_name().unwrap())
        }
        Err(error) => return Err(error.into()),
    };
    if resolved == base || !resolved.starts_with(&base) {
        return Err(Error::bad("File path escapes its skill directory."));
    }
    Ok(resolved)
}

pub async fn workspace(path: &Path, roots: &[PathBuf]) -> Result<PathBuf> {
    let actual = fs::canonicalize(path)
        .await
        .map_err(|_| Error::bad("Project directory does not exist or is not accessible."))?;
    if !fs::metadata(&actual).await?.is_dir() {
        return Err(Error::bad(
            "Project directory does not exist or is not accessible.",
        ));
    }
    for root in roots {
        if let Ok(root) = fs::canonicalize(root).await
            && actual.starts_with(root)
        {
            return Ok(actual);
        }
    }
    Err(Error::bad(
        "Project must be inside a configured workspace root.",
    ))
}

pub async fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(path).await?;
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(())
}

pub async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_file_name(format!(".{}.tmp", crate::config::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .await?;
    file.write_all(bytes).await?;
    file.sync_all().await?;
    drop(file);
    fs::rename(&temporary, path).await?;
    Ok(())
}

pub async fn small_file(path: &Path) -> Result<String> {
    let bytes = bounded_file(path, 100000).await?;
    String::from_utf8(bytes).map_err(|_| Error::bad("File must contain UTF-8 text."))
}

/// Reads a regular file of at most `limit` bytes without following a link.
async fn bounded_file(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .await?;
    let mut bytes = Vec::new();
    (&mut file).take(limit + 1).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 > limit {
        return Err(Error::bad("File is too large."));
    }
    Ok(bytes)
}

/// Decodes request content sent as `utf-8` (the default) or `base64`.
pub fn decode(content: &str, encoding: Option<&str>) -> Result<Vec<u8>> {
    match encoding {
        None | Some("utf-8") => Ok(content.as_bytes().to_vec()),
        Some("base64") => STANDARD
            .decode(content)
            .map_err(|_| Error::bad("Invalid base64 content.")),
        Some(_) => Err(Error::bad("encoding: use utf-8 or base64")),
    }
}

/// Whether an imported path belongs in a skill: no hidden names and no macOS
/// archive metadata.
pub fn visible(path: &str) -> bool {
    !path.starts_with("__MACOSX/") && !path.split('/').any(|segment| segment.starts_with('.'))
}

/// The skill folder an import should use.
pub enum Choice {
    Folder(String),
    Candidates(Vec<String>),
}

/// Picks the folder holding `SKILL.md` among `files` (paths relative to the
/// archive or repository), honouring an explicitly requested folder.
pub fn choose<'a>(
    files: impl IntoIterator<Item = &'a str>,
    requested: Option<&str>,
    origin: &str,
) -> Result<Choice> {
    let mut folders = files
        .into_iter()
        .filter_map(|path| match path {
            "SKILL.md" => Some(String::new()),
            _ => path.strip_suffix("/SKILL.md").map(str::to_owned),
        })
        .collect::<Vec<_>>();
    folders.sort();
    folders.dedup();

    let requested = requested
        .map(|value| value.trim_matches('/'))
        .filter(|value| !value.is_empty());
    if let Some(requested) = requested {
        if folders.iter().any(|folder| folder == requested) {
            return Ok(Choice::Folder(requested.to_owned()));
        }
        folders.retain(|folder| folder.starts_with(&format!("{requested}/")));
        if folders.is_empty() {
            return Err(Error::bad(format!("No SKILL.md found in {requested}.")));
        }
    } else if folders.iter().any(String::is_empty) {
        return Ok(Choice::Folder(String::new()));
    }
    match folders.len() {
        0 => Err(Error::bad(format!("No SKILL.md found in {origin}."))),
        1 => Ok(Choice::Folder(folders.remove(0))),
        _ => {
            folders.truncate(200);
            Ok(Choice::Candidates(folders))
        }
    }
}

/// Removes entries of `directory` older than `age` milliseconds.
async fn expire(directory: &Path, age: i64) {
    let Ok(mut items) = fs::read_dir(directory).await else {
        return;
    };
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_millis(age as u64);
    while let Ok(Some(item)) = items.next_entry().await {
        let Ok(metadata) = item.metadata().await else {
            continue;
        };
        if metadata.modified().is_ok_and(|modified| modified < cutoff) {
            let _ = if metadata.is_dir() {
                fs::remove_dir_all(item.path()).await
            } else {
                fs::remove_file(item.path()).await
            };
        }
    }
}

const HOUR: i64 = 3_600_000;
const ARCHIVE_CHUNK: usize = 512 * 1024;

/// The frozen copy of a skill revision, when `revision` is a revision.
pub fn frozen(config: &Config, revision: &str) -> Option<PathBuf> {
    (revision.len() == 64 && revision.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| config.data_dir.join("skill-bundles").join(revision))
}

/// The wire description of one version, captured while reading or saving it.
pub fn bundle_response(scope: &str, skill: &str, described: &bundle::Described) -> Value {
    json!({
        "scope": scope,
        "name": skill,
        "description": described.description,
        "revision": described.revision,
        "files": described.files,
        "limits": bundle::limits(),
        "source": null,
    })
}

async fn skill_directory(root: &Path, skill: &str) -> Result<PathBuf> {
    name(skill)?;
    bounded(root, skill, false).await
}

#[derive(Clone)]
pub struct Skills {
    pub config: Config,
    pub(crate) mutations: std::sync::Arc<tokio::sync::Mutex<()>>,
}

impl Skills {
    pub async fn root(&self, project: Option<&Path>) -> Result<PathBuf> {
        let mut current = if let Some(project) = project {
            workspace(project, &self.config.workspace_roots).await?
        } else {
            fs::canonicalize(&self.config.home).await?
        };
        for segment in [".agents", "skills"] {
            current.push(segment);
            match fs::create_dir(&current).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            };
            let info = fs::symlink_metadata(&current).await?;
            if !info.is_dir() || info.is_symlink() {
                return Err(Error::bad(
                    "The .agents/skills path must use real directories, not symbolic links.",
                ));
            }
        }
        Ok(current)
    }

    pub async fn list(&self, scope: &str, project: Option<&Path>) -> Result<Vec<Value>> {
        let root = self.root(project).await?;
        let _read = bundle::access(&root).await.read_owned().await;
        let mut entries = fs::read_dir(&root).await?;
        let mut listings = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let directory = entry.file_name().to_string_lossy().into_owned();
            // Hidden folders hold in-progress saves.
            if !entry.file_type().await?.is_dir() || directory.starts_with('.') {
                continue;
            }
            let mut listing = cairn_contracts::Skill {
                name: directory.clone(),
                description: "Invalid skill".into(),
                scope: scope.to_owned(),
                path: root
                    .join(&directory)
                    .join("SKILL.md")
                    .to_string_lossy()
                    .into_owned(),
                content: String::new(),
                valid: false,
                error: None,
                files: 0,
                bytes: 0,
                executables: 0,
                usage_count: 0,
                source: None,
            };
            let read = async {
                name(&directory)?;
                let path = bounded(&root, &format!("{directory}/SKILL.md"), false).await?;
                let content = small_file(&path).await?;
                Ok::<_, Error>((path, content))
            }
            .await;
            match read {
                Ok((path, content)) => read_listing(&mut listing, &directory, &path, content),
                Err(error) => listing.error = Some(error.message),
            }
            match bundle::scan(&root.join(&directory)).await {
                Ok(files) => {
                    listing.files = files.len() as u32;
                    listing.bytes = files.iter().map(|file| file.size).sum();
                    listing.executables =
                        files.iter().filter(|file| file.executable).count() as u32;
                }
                Err(error) => {
                    listing.valid = false;
                    if listing.error.is_none() {
                        listing.error = Some(error.message);
                    }
                }
            }
            listings.push(listing);
        }
        listings.sort_by(|a, b| a.name.cmp(&b.name));
        listings
            .into_iter()
            .map(|listing| Ok(serde_json::to_value(listing)?))
            .collect()
    }

    /// Writes `SKILL.md`, creating the skill when it does not exist.
    pub async fn save(&self, skill: &str, content: &str, project: Option<&Path>) -> Result<Value> {
        self.save_authorized(skill, content, project, None).await
    }

    pub(crate) async fn save_authorized(
        &self,
        skill: &str,
        content: &str,
        project: Option<&Path>,
        authorization: Option<bundle::Authorization<'_>>,
    ) -> Result<Value> {
        name(skill)?;
        let parsed = metadata(content)?;
        if parsed.name != skill {
            return Err(Error::bad(
                "Frontmatter name must match the skill directory.",
            ));
        }
        let root = self.root(project).await?;
        let directory = root.join(skill);
        let revision = match fs::symlink_metadata(&directory).await {
            Ok(_) => Some(bundle::describe(&directory).await?.revision),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let change = Change::Write {
            path: "SKILL.md".to_owned(),
            bytes: content.as_bytes().to_vec(),
            executable: None,
        };
        self.apply_authorized(
            skill,
            revision.as_deref(),
            &[change],
            project,
            authorization,
        )
        .await?;
        Ok(serde_json::to_value(parsed)?)
    }

    /// Text files as UTF-8, other files as base64.
    pub async fn read(&self, skill: &str, file: &str, project: Option<&Path>) -> Result<Value> {
        let root = self.root(project).await?;
        let _read = bundle::access(&root).await.read_owned().await;
        if !visible(file) {
            return Err(Error::bad(
                "Files whose names start with . are not part of a skill.",
            ));
        }
        let directory = skill_directory(&root, skill).await?;
        let target = bounded(&directory, file, false).await?;
        let metadata = fs::symlink_metadata(&target).await?;
        if !metadata.is_file() {
            return Err(Error::bad(format!("{file} is not a file.")));
        }
        let bytes = bounded_file(&target, bundle::FILE_BYTES)
            .await
            .map_err(|_| Error::too_large(format!("{file} is too large to open here.")))?;
        let executable = metadata.permissions().mode() & 0o111 != 0;
        let size = bytes.len();
        let (content, encoding) = if bundle::is_text(&bytes) {
            (String::from_utf8(bytes).unwrap(), "utf-8")
        } else {
            (STANDARD.encode(bytes), "base64")
        };
        Ok(json!({
            "content": content,
            "encoding": encoding,
            "size": size,
            "executable": executable,
        }))
    }

    pub async fn bundle(&self, scope: &str, skill: &str, project: Option<&Path>) -> Result<Value> {
        let root = self.root(project).await?;
        let _read = bundle::access(&root).await.read_owned().await;
        let directory = skill_directory(&root, skill).await?;
        let described = bundle::describe_locked(&directory).await?;
        Ok(bundle_response(scope, skill, &described))
    }

    /// Resolves canonical batch changes, reading staged uploads. Returns the
    /// upload ids to discard once the batch succeeds.
    pub(crate) async fn changes(
        &self,
        items: Vec<cairn_contracts::SkillChange>,
    ) -> Result<(Vec<Change>, Vec<String>)> {
        use cairn_contracts::SkillChange;

        if items.len() > 4 * bundle::FILES {
            return Err(Error::bad("Too many changes in one save."));
        }
        let mut changes = Vec::with_capacity(items.len());
        let mut uploads = Vec::new();
        let mut written = 0;
        for item in items {
            let change = match item {
                SkillChange::Write {
                    path,
                    content,
                    encoding,
                    upload,
                    executable,
                } => {
                    let bytes = if let Some(upload) = upload {
                        if uploads.iter().any(|used| used == &upload) {
                            return Err(Error::bad("Each upload can be used once."));
                        }
                        let bytes = self.upload(&upload).await?;
                        uploads.push(upload);
                        bytes
                    } else {
                        let content = content
                            .as_deref()
                            .ok_or_else(|| Error::bad("content: expected text"))?;
                        decode(content, encoding.as_deref())?
                    };
                    written += bytes.len() as u64;
                    if written > 2 * bundle::BUNDLE_BYTES {
                        return Err(Error::too_large("This save writes too much at once."));
                    }
                    Change::Write {
                        path,
                        bytes,
                        executable,
                    }
                }
                SkillChange::Delete { path } => Change::Delete { path },
                SkillChange::Move { from, to } => Change::Move { from, to },
                SkillChange::Mode { path, executable } => Change::Mode { path, executable },
            };
            changes.push(change);
        }
        Ok((changes, uploads))
    }

    /// Applies a batch; `base` is `None` to create the skill.
    pub async fn apply(
        &self,
        skill: &str,
        base: Option<&str>,
        changes: &[Change],
        project: Option<&Path>,
    ) -> Result<bundle::Described> {
        self.apply_authorized(skill, base, changes, project, None)
            .await
    }

    pub(crate) async fn apply_authorized(
        &self,
        skill: &str,
        base: Option<&str>,
        changes: &[Change],
        project: Option<&Path>,
        authorization: Option<bundle::Authorization<'_>>,
    ) -> Result<bundle::Described> {
        name(skill)?;
        let root = self.root(project).await?;
        let base = base.map_or(Base::Create, Base::Edit);
        bundle::save_authorized(&root, skill, base, changes, authorization).await
    }

    fn uploads(&self) -> PathBuf {
        self.config.data_dir.join("skill-uploads")
    }

    /// Stages a file for a later batch or import.
    pub async fn stage(&self, bytes: &[u8]) -> Result<Value> {
        if bytes.len() > bundle::UPLOAD_BYTES {
            return Err(Error::too_large(format!(
                "Uploads are limited to {} MiB.",
                bundle::UPLOAD_BYTES >> 20
            )));
        }
        let directory = self.uploads();
        private_dir(&directory).await?;
        expire(&directory, HOUR).await;
        let id = crate::config::id();
        atomic_write(&directory.join(&id), bytes).await?;
        Ok(json!({ "id": id, "size": bytes.len() }))
    }

    pub async fn upload(&self, id: &str) -> Result<Vec<u8>> {
        uuid::Uuid::parse_str(id).map_err(|_| Error::bad("Invalid upload."))?;
        match fs::read(self.uploads().join(id)).await {
            Ok(bytes) => Ok(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::bad("This upload expired. Add the file again."))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn discard(&self, uploads: &[String]) {
        for id in uploads {
            if uuid::Uuid::parse_str(id).is_ok() {
                let _ = fs::remove_file(self.uploads().join(id)).await;
            }
        }
    }

    /// One chunk of the skill as a ZIP archive whose top folder is the skill.
    pub async fn archive(
        &self,
        skill: &str,
        revision: Option<&str>,
        offset: u64,
        project: Option<&Path>,
    ) -> Result<Value> {
        let root = self.root(project).await?;
        let _read = bundle::access(&root).await.read_owned().await;
        let directory = skill_directory(&root, skill).await?;
        let (current, files) = bundle::read_all_locked(&directory).await?;
        drop(_read);
        if revision.is_some_and(|revision| revision != current) {
            return Err(Error::conflict(
                "This skill changed during the download. Try again.",
            ));
        }

        let cache = self.config.data_dir.join("skill-archives");
        private_dir(&cache).await?;
        let file = cache.join(format!("{current}.zip"));
        if !fs::try_exists(&file).await? {
            expire(&cache, HOUR).await;
            let prefix = skill.to_owned();
            let archive = tokio::task::spawn_blocking(move || {
                let paths = files
                    .iter()
                    .map(|(entry, _)| format!("{prefix}/{}", entry.path))
                    .collect::<Vec<_>>();
                let entries = files
                    .iter()
                    .zip(&paths)
                    .map(|((entry, bytes), path)| zip::File {
                        path,
                        bytes,
                        executable: entry.executable,
                    })
                    .collect::<Vec<_>>();
                zip::write(&entries)
            })
            .await
            .map_err(|_| Error::bad("Could not build the archive."))??;
            atomic_write(&file, &archive).await?;
        }

        let mut reader = fs::File::open(&file).await?;
        let size = reader.metadata().await?.len();
        if offset > size {
            return Err(Error::bad("offset: beyond the end of the archive"));
        }
        reader.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut chunk = Vec::new();
        reader
            .take(ARCHIVE_CHUNK as u64)
            .read_to_end(&mut chunk)
            .await?;
        Ok(json!({
            "filename": format!("{skill}.zip"),
            "revision": current,
            "size": size,
            "offset": offset,
            "content": STANDARD.encode(chunk),
        }))
    }

    /// Creates a skill from a staged `.zip` or `.skill` archive.
    pub(crate) async fn import_archive(
        &self,
        scope: &str,
        upload: &str,
        requested: Option<&str>,
        project: Option<&Path>,
        authorization: bundle::Authorization<'_>,
    ) -> Result<Value> {
        let archive = self.upload(upload).await?;
        let entries = zip::entries(&archive)?;
        let visible = entries
            .iter()
            .filter(|entry| visible(&entry.path))
            .collect::<Vec<_>>();
        let folder = match choose(
            visible.iter().map(|entry| entry.path.as_str()),
            requested,
            "the archive",
        )? {
            Choice::Candidates(candidates) => {
                return Ok(json!({ "imported": null, "candidates": candidates }));
            }
            Choice::Folder(folder) => folder,
        };

        let prefix = if folder.is_empty() {
            String::new()
        } else {
            format!("{folder}/")
        };
        let selected = visible
            .into_iter()
            .filter_map(|entry| Some((entry.path.strip_prefix(&prefix)?, entry)))
            .collect::<Vec<_>>();
        check_sizes(selected.iter().map(|(_, entry)| entry.size))?;
        let mut changes = Vec::with_capacity(selected.len());
        for (path, entry) in selected {
            changes.push(Change::Write {
                path: path.to_owned(),
                bytes: zip::extract(&archive, entry)?,
                executable: entry.executable,
            });
        }

        let skill = imported_name(&changes)?;
        let saved = self
            .apply_authorized(&skill, None, &changes, project, Some(authorization))
            .await?;
        self.discard(&[upload.to_owned()]).await;
        Ok(json!({
            "imported": bundle_response(scope, &skill, &saved),
            "candidates": [],
        }))
    }

    /// Copies a skill into the content-addressed store so a run installs
    /// exactly what it was created with. Returns the revision and `SKILL.md`.
    pub async fn freeze(&self, skill_file: &Path) -> Result<(String, String)> {
        let directory = skill_file
            .parent()
            .ok_or_else(|| Error::bad("Invalid selected skill path."))?;
        let (revision, files) = bundle::read_all(directory).await?;
        let content = files
            .iter()
            .find(|(entry, _)| entry.path == "SKILL.md")
            .and_then(|(_, bytes)| String::from_utf8(bytes.clone()).ok())
            .ok_or_else(|| Error::bad("Selected skill has no readable SKILL.md."))?;

        let store = self.config.data_dir.join("skill-bundles");
        private_dir(&store).await?;
        let target = store.join(&revision);
        if !fs::try_exists(&target).await? {
            let temporary = store.join(format!(".tmp-{}", crate::config::id()));
            let copied = async {
                private_dir(&temporary).await?;
                for (entry, bytes) in &files {
                    bundle::parents(&temporary, &entry.path).await?;
                    bundle::write_file(&temporary.join(&entry.path), bytes, entry.executable)
                        .await?;
                }
                Ok::<_, Error>(())
            }
            .await;
            let renamed = match copied {
                Ok(()) => fs::rename(&temporary, &target).await.map_err(Error::from),
                Err(error) => Err(error),
            };
            if let Err(error) = renamed {
                let _ = fs::remove_dir_all(&temporary).await;
                if !fs::try_exists(&target).await? {
                    return Err(error);
                }
            }
        }
        // Completed bundles are immutable snapshot data. Age does not prove that
        // queued runs or dormant conversations have released their references.
        Ok((revision, content))
    }
}

/// Refuses imports beyond the bundle limits before extracting anything.
pub fn check_sizes(sizes: impl IntoIterator<Item = u64>) -> Result<()> {
    let (mut count, mut total) = (0, 0);
    for size in sizes {
        count += 1;
        total += size;
        if size > bundle::FILE_BYTES {
            return Err(Error::too_large(format!(
                "Skill files are limited to {} MiB each.",
                bundle::FILE_BYTES >> 20
            )));
        }
    }
    if count > bundle::FILES || total > bundle::BUNDLE_BYTES {
        return Err(Error::too_large(format!(
            "A skill can contain at most {} files and {} MiB.",
            bundle::FILES,
            bundle::BUNDLE_BYTES >> 20
        )));
    }
    Ok(())
}

/// The skill name declared by the `SKILL.md` among imported files.
pub fn imported_name(changes: &[Change]) -> Result<String> {
    let content = changes
        .iter()
        .find_map(|change| match change {
            Change::Write { path, bytes, .. } if path == "SKILL.md" => Some(bytes),
            _ => None,
        })
        .ok_or_else(|| Error::bad("The imported folder has no SKILL.md."))?;
    let content =
        std::str::from_utf8(content).map_err(|_| Error::bad("SKILL.md must be UTF-8 text."))?;
    Ok(metadata(content)?.name)
}
