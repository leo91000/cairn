use crate::{
    error::{Error, Result},
    process::{Environment, bounded_output, command},
};
use std::{path::Path, time::Duration};

async fn copy_new(source: &Path, target: &Path, optional: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut source = match tokio::fs::File::open(source).await {
        Ok(source) => source,
        Err(e) if optional && e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mode = source.metadata().await?.permissions().mode();
    let mut target = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(target)
        .await
    {
        Ok(target) => target,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    tokio::io::copy(&mut source, &mut target).await?;
    Ok(())
}

async fn link(source: &Path, target: &Path) -> Result<()> {
    match tokio::fs::symlink(source, target).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}

async fn names(path: &Path) -> Result<Vec<String>> {
    let mut entries = tokio::fs::read_dir(path).await?;
    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        names.push(
            entry
                .file_name()
                .to_str()
                .ok_or_else(|| Error::internal("Invalid toolkit filename"))?
                .to_owned(),
        );
    }
    Ok(names)
}

async fn prune(path: &Path, directory: &Path) -> Result<()> {
    for name in names(path).await? {
        let file = path.join(name);
        if let Ok(target) = tokio::fs::read_link(&file).await
            && (target.starts_with("/usr/local/share/mise/installs")
                || target.starts_with(directory.join("rustup/toolchains")))
            && !tokio::fs::try_exists(&file).await?
        {
            tokio::fs::remove_file(file).await?;
        }
    }
    Ok(())
}

pub async fn environment(home: &Path, mut env: Environment) -> Result<Environment> {
    let Some(directory) = env.get("LEO_TOOLKIT_DIR").cloned() else {
        return Ok(env);
    };
    let directory = Path::new(&directory);
    let rustup = home.join(".rustup");
    let cargo = home.join(".cargo");
    for path in [
        rustup.join("toolchains"),
        cargo.join("bin"),
        rustup.join("update-hashes"),
        rustup.join("downloads"),
        rustup.join("tmp"),
    ] {
        tokio::fs::create_dir_all(path).await?;
    }
    for name in names(&directory.join("rustup/update-hashes")).await? {
        copy_new(
            &directory.join("rustup/update-hashes").join(&name),
            &rustup.join("update-hashes").join(name),
            false,
        )
        .await?;
    }
    prune(&rustup.join("toolchains"), directory).await?;
    let installs = home.join(".local/share/mise/installs");
    for tool in ["node", "pnpm", "python", "go", "rust", "java"] {
        let source = Path::new("/usr/local/share/mise/installs").join(tool);
        let target = installs.join(tool);
        tokio::fs::create_dir_all(&target).await?;
        prune(&target, directory).await?;
        copy_new(
            &source.join(".mise.backend.toml"),
            &target.join(".mise.backend.toml"),
            true,
        )
        .await?;
        for version in names(&source).await? {
            let parts = version.split('.').collect::<Vec<_>>();
            if tool == "java" && version.starts_with("temurin-")
                || parts.len() == 3
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            {
                link(&source.join(&version), &target.join(version)).await?;
            }
        }
    }
    let shims = home.join(".local/share/mise/shims");
    tokio::fs::create_dir_all(&shims).await?;
    for name in names(Path::new("/usr/local/share/mise/shims")).await? {
        link(Path::new("/usr/local/bin/mise"), &shims.join(name)).await?;
    }
    for name in names(&directory.join("rustup/toolchains")).await? {
        link(
            &directory.join("rustup/toolchains").join(&name),
            &rustup.join("toolchains").join(name),
        )
        .await?;
    }
    for name in names(&directory.join("cargo/bin")).await? {
        let target = cargo.join("bin").join(&name);
        if tokio::fs::read_link(&target)
            .await
            .is_ok_and(|p| p.starts_with(directory.join("cargo/bin")))
        {
            tokio::fs::remove_file(&target).await?;
        }
        if name == "rustup" {
            copy_new(&directory.join("cargo/bin/rustup"), &target, false).await?;
        } else {
            link(&cargo.join("bin/rustup"), &target).await?;
        }
    }
    copy_new(
        &directory.join("rustup/settings.toml"),
        &rustup.join("settings.toml"),
        false,
    )
    .await?;
    env.insert(
        "ANDROID_HOME".into(),
        home.join(".local/share/android/sdk")
            .to_string_lossy()
            .into_owned(),
    );
    env.insert(
        "ANDROID_USER_HOME".into(),
        home.join(".android").to_string_lossy().into_owned(),
    );
    env.insert(
        "GRADLE_USER_HOME".into(),
        home.join(".gradle").to_string_lossy().into_owned(),
    );
    env.insert("HOME".into(), home.to_string_lossy().into_owned());
    env.insert("RUSTUP_HOME".into(), rustup.to_string_lossy().into_owned());
    env.insert("CARGO_HOME".into(), cargo.to_string_lossy().into_owned());
    env.insert(
        "PATH".into(),
        format!(
            "{}:/usr/local/share/mise/shims:{}:{}/.local/share/android/sdk/platform-tools:{}/.local/share/android/sdk/cmdline-tools/latest/bin:{}",
            shims.display(),
            cargo.join("bin").display(),
            home.display(),
            home.display(),
            env.get("PATH")
                .map(String::as_str)
                .unwrap_or("/usr/local/bin:/usr/bin:/bin")
        ),
    );
    for args in [vec!["reshim".into()], vec!["env".into(), "--json".into()]] {
        let output = bounded_output(
            command("/usr/local/bin/mise", &args, &env, Some(Path::new("/tmp"))),
            Duration::from_secs(30),
            100000,
        )
        .await?;
        if !output.success {
            return Err(Error::new(503, "Unable to prepare the agent toolkit."));
        }
    }
    Ok(env)
}
