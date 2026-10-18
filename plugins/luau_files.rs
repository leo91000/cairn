//! Shared recursive inventory for bundled plugin builds and runtime tests.
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn luau(root: &Path, directory: &Path, files: &mut Vec<(String, PathBuf)>) {
    let mut entries = fs::read_dir(directory)
        .expect("plugin directory")
        .map(|entry| entry.expect("plugin entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            luau(root, &path, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "luau")
        {
            let relative = path.strip_prefix(root).expect("inside the package");
            let name = relative
                .components()
                .map(|part| part.as_os_str().to_str().expect("UTF-8 file name"))
                .collect::<Vec<_>>()
                .join("/");
            files.push((name, path));
        }
    }
}
