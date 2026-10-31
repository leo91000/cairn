//! A package's Luau files: `main.luau`, the entry point, and the modules it loads with
//! `require("./path")`. Workers receive them all at load; nothing is read from disk.
use std::collections::BTreeMap;

/// Largest package: every Luau file together. Frames stay under `MAX_FRAME_BYTES`
/// even if escaping doubles the text.
pub const MAX_SOURCE_BYTES: usize = 393_216;
/// Most modules a package may have besides `main.luau`.
pub const MAX_MODULES: usize = 64;
const MAX_DEPTH: usize = 4;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sources {
    /// `main.luau`: returns the package's table of handlers.
    pub main: String,
    /// Other files by path relative to the package, e.g. `lib/rest.luau`.
    pub modules: BTreeMap<String, String>,
}

impl From<&str> for Sources {
    fn from(main: &str) -> Self {
        Self {
            main: main.to_owned(),
            modules: BTreeMap::new(),
        }
    }
}

impl From<&Sources> for Sources {
    fn from(sources: &Sources) -> Self {
        sources.clone()
    }
}

/// `lib/rest.luau`: lowercase segments, at most four levels, ending in `.luau`.
pub fn module_path(path: &str) -> bool {
    let Some(stem) = path.strip_suffix(".luau") else {
        return false;
    };
    let segments = stem.split('/').collect::<Vec<_>>();
    path.len() <= 120
        && path != "main.luau"
        && segments.len() <= MAX_DEPTH
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && segment.len() <= 40
                && segment.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                && segment.bytes().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-')
                })
        })
}

impl Sources {
    /// The exact manifest and files loaded by a worker, using the registry's
    /// approval format. Keep this stable for packages without new declarations.
    pub fn digest(&self, manifest: &crate::Manifest) -> serde_json::Result<String> {
        use sha2::{Digest, Sha256};
        let files = self.files().collect::<BTreeMap<_, _>>();
        let material = format!(
            "cairn-package\n{}\n{}",
            serde_json::to_string(manifest)?,
            serde_json::to_string(&files)?
        );
        Ok(format!("{:x}", Sha256::digest(material.as_bytes())))
    }

    /// Bytes of Luau in the whole package.
    pub fn size(&self) -> usize {
        self.main.len() + self.modules.values().map(String::len).sum::<usize>()
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.size() > MAX_SOURCE_BYTES {
            return Err("Plugin source is too large.");
        }
        if self.modules.len() > MAX_MODULES {
            return Err("Plugin package has too many files.");
        }
        if !self.modules.keys().all(|path| module_path(path)) {
            return Err("Plugin file names must be like lib/name.luau.");
        }
        Ok(())
    }

    /// Every file with its path, `main.luau` first.
    pub fn files(&self) -> impl Iterator<Item = (&str, &str)> {
        std::iter::once(("main.luau", self.main.as_str())).chain(
            self.modules
                .iter()
                .map(|(path, source)| (path.as_str(), source.as_str())),
        )
    }
}

/// Resolves `require("./x")` or `require("../x")` from the module at `from`, Luau
/// style: relative to the requiring file, never above the package. `x.luau` first,
/// then `x/init.luau`.
pub fn resolve(sources: &Sources, from: &str, request: &str) -> Option<String> {
    if !(request.starts_with("./") || request.starts_with("../")) || request.ends_with('/') {
        return None;
    }
    let mut parts = from.split('/').collect::<Vec<_>>();
    parts.pop();
    for (index, segment) in request.split('/').enumerate() {
        match segment {
            "." if index == 0 => {}
            ".." => {
                parts.pop()?;
            }
            "" | "." => return None,
            segment => parts.push(segment),
        }
    }
    let path = parts.join("/");
    [format!("{path}.luau"), format!("{path}/init.luau")]
        .into_iter()
        .find(|candidate| sources.modules.contains_key(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources() -> Sources {
        Sources {
            main: "return {}".into(),
            modules: [
                ("gateway.luau", ""),
                ("lib/rest.luau", ""),
                ("lib/text.luau", ""),
                ("lib/format/init.luau", ""),
            ]
            .into_iter()
            .map(|(path, source)| (path.to_owned(), source.to_owned()))
            .collect(),
        }
    }

    #[test]
    fn requires_resolve_relative_to_the_requiring_file_inside_the_package() {
        let sources = sources();
        let resolve = |from: &str, request: &str| resolve(&sources, from, request);
        assert_eq!(
            resolve("main.luau", "./gateway").as_deref(),
            Some("gateway.luau")
        );
        assert_eq!(
            resolve("main.luau", "./lib/rest").as_deref(),
            Some("lib/rest.luau")
        );
        assert_eq!(
            resolve("lib/rest.luau", "./text").as_deref(),
            Some("lib/text.luau")
        );
        assert_eq!(
            resolve("lib/rest.luau", "../gateway").as_deref(),
            Some("gateway.luau")
        );
        assert_eq!(
            resolve("main.luau", "./lib/format").as_deref(),
            Some("lib/format/init.luau")
        );
        for (from, request) in [
            ("main.luau", "gateway"),
            ("main.luau", "../gateway"),
            ("main.luau", "./main"),
            ("main.luau", "@cairn/http"),
            ("main.luau", "./lib/../../gateway"),
            ("main.luau", "./lib//rest"),
            ("main.luau", "/etc/passwd"),
            ("main.luau", "./missing"),
        ] {
            assert_eq!(resolve(from, request), None, "{request}");
        }
    }

    #[test]
    fn packages_bound_their_files_names_and_size() {
        sources().validate().unwrap();
        for path in [
            "Main.luau",
            "lib/rest.lua",
            "../x.luau",
            "a/b/c/d/e.luau",
            "main.luau",
            ".x.luau",
            "lib/.luau",
        ] {
            assert!(!module_path(path), "{path}");
        }
        let mut large = sources();
        large.main = "x".repeat(MAX_SOURCE_BYTES + 1);
        assert!(large.validate().is_err());
        let mut many = sources();
        for index in 0..=MAX_MODULES {
            many.modules.insert(format!("m{index}.luau"), String::new());
        }
        assert!(many.validate().is_err());
    }
}
