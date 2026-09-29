use crate::{
    config::Config,
    error::{Error, Result},
    validation::text,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
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

pub fn parse(content: &str) -> Result<Value> {
    if content.len() > 100000 {
        return Err(Error::bad("Skill is too large (maximum 100 KB)."));
    }
    let normalized = content.replace("\r\n", "\n");
    let body = normalized
        .strip_prefix("---\n")
        .ok_or_else(|| Error::bad("SKILL.md needs YAML frontmatter with name and description."))?;
    let end = body
        .match_indices("\n---")
        .find(|(index, _)| {
            let rest = &body[index + 4..];
            rest.is_empty() || rest.starts_with('\n')
        })
        .map(|(index, _)| index)
        .ok_or_else(|| Error::bad("SKILL.md needs YAML frontmatter with name and description."))?;
    let front = &body[..end];
    let data: Value =
        serde_yaml_ng::from_str(front).map_err(|_| Error::bad("Invalid YAML frontmatter."))?;
    if !data["name"].is_string() || text(&data, "description").trim().is_empty() {
        return Err(Error::bad(
            "Add a name and description to the skill frontmatter.",
        ));
    }
    name(text(&data, "name"))?;
    Ok(json!({
        "name": data["name"],
        "description": data["description"]
    }))
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
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .await?;
    let mut bytes = Vec::new();
    (&mut file).take(100001).read_to_end(&mut bytes).await?;
    if bytes.len() > 100000 {
        return Err(Error::bad("File is too large."));
    }
    String::from_utf8(bytes).map_err(|_| Error::bad("File must contain UTF-8 text."))
}

#[derive(Clone)]
pub struct Skills {
    pub config: Config,
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
                Ok(_) => {}
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
        let mut entries = fs::read_dir(&root).await?;
        let mut result = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let filename = entry.file_name().to_string_lossy().into_owned();
            let mut value = json!({
                "name": filename,
                "description": "Invalid skill",
                "scope": scope,
                "path": root.join(&filename).join("SKILL.md"),
                "content": "",
                "valid": false
            });
            let read = async {
                name(&filename)?;
                let path = bounded(&root, &format!("{filename}/SKILL.md"), false).await?;
                let content = small_file(&path).await?;
                Ok::<_, Error>((path, content))
            }
            .await;
            match read {
                Ok((path, content)) => {
                    value["path"] = path.to_string_lossy().into_owned().into();
                    value["content"] = content.clone().into();
                    match parse(&content) {
                        Ok(parsed) => {
                            value["name"] = parsed["name"].clone();
                            value["description"] = parsed["description"].clone();
                            value["valid"] = (parsed["name"] == filename).into();
                            if parsed["name"] != filename {
                                value["error"] = "Name differs from directory".into();
                            }
                        }
                        Err(error) => value["error"] = error.message.into(),
                    }
                }
                Err(error) => value["error"] = error.message.into(),
            };
            result.push(value);
        }
        result.sort_by(|a, b| text(a, "name").cmp(text(b, "name")));
        Ok(result)
    }

    pub async fn save(&self, skill: &str, content: &str, project: Option<&Path>) -> Result<Value> {
        name(skill)?;
        let parsed = parse(content)?;
        if parsed["name"] != skill {
            return Err(Error::bad(
                "Frontmatter name must match the skill directory.",
            ));
        }
        let root = self.root(project).await?;
        let directory = bounded(&root, skill, true).await?;
        private_dir(&directory).await?;
        let file = bounded(&root, &format!("{skill}/SKILL.md"), true).await?;
        atomic_write(&file, content.as_bytes()).await?;
        Ok(parsed)
    }

    pub async fn remove(&self, skill: &str, project: Option<&Path>) -> Result<()> {
        name(skill)?;
        let directory = bounded(&self.root(project).await?, skill, false).await?;
        fs::remove_dir_all(directory).await?;
        Ok(())
    }

    pub async fn files(&self, skill: &str, project: Option<&Path>) -> Result<Vec<String>> {
        name(skill)?;
        let root = bounded(&self.root(project).await?, skill, false).await?;
        let mut paths = vec![(PathBuf::new(), 0)];
        let mut result = Vec::new();
        while let Some((relative, depth)) = paths.pop() {
            let mut entries = fs::read_dir(root.join(&relative)).await?;
            while let Some(entry) = entries.next_entry().await? {
                if result.len() >= 200 {
                    return Ok(result);
                }
                let name = entry.file_name();
                if name.to_string_lossy().starts_with('.') || entry.file_type().await?.is_symlink()
                {
                    continue;
                }
                let file = relative.join(name);
                if entry.file_type().await?.is_dir() {
                    if depth < 3 {
                        paths.push((file, depth + 1));
                    }
                } else {
                    result.push(file.to_string_lossy().into_owned());
                }
            }
        }
        Ok(result)
    }

    pub async fn file(
        &self,
        skill: &str,
        file: &str,
        content: Option<&str>,
        project: Option<&Path>,
    ) -> Result<Value> {
        name(skill)?;
        relative(file)?;
        let root = bounded(&self.root(project).await?, skill, false).await?;
        if content.is_some() {
            let mut directory = root.clone();
            let parts = file.split('/').collect::<Vec<_>>();
            for part in &parts[..parts.len() - 1] {
                directory = bounded(&directory, part, true).await?;
                private_dir(&directory).await?;
                bounded(
                    &root,
                    directory
                        .strip_prefix(&root)
                        .map_err(|_| Error::bad("Invalid file path."))?
                        .to_str()
                        .ok_or_else(|| Error::bad("Invalid file path."))?,
                    false,
                )
                .await?;
            }
        }
        let target = bounded(&root, file, content.is_some()).await?;
        let Some(content) = content else {
            return Ok(json!({
                "content": small_file(&target).await?
            }));
        };
        if content.len() > 100000 {
            return Err(Error::bad("File is too large."));
        }
        if file == "SKILL.md" {
            return self.save(skill, content, project).await;
        }
        atomic_write(&target, content.as_bytes()).await?;
        Ok(json!({
            "saved": true
        }))
    }
}
