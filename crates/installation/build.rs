//! Embeds the bundled plugin packages (`plugins/<id>/`): each manifest and every Luau
//! file, so a package can be split into modules without listing them here.
use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

#[path = "../../plugins/luau_files.rs"]
mod luau_files;

fn main() {
    let plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    println!("cargo:rerun-if-changed={}", plugins.display());
    let mut packages = fs::read_dir(&plugins)
        .expect("plugins directory")
        .map(|entry| entry.expect("plugins entry").path())
        .filter(|path| path.join("manifest.json").is_file() || path.join("manifest.toml").is_file())
        .collect::<Vec<_>>();
    packages.sort();
    let mut code = String::from("&[\n");
    for package in packages {
        let id = package.file_name().unwrap().to_str().unwrap();
        let mut files = Vec::new();
        luau_files::luau(&package, &package, &mut files);
        let json = package.join("manifest.json");
        let toml = package.join("manifest.toml");
        let (format, manifest) = match (json.is_file(), toml.is_file()) {
            (true, false) => ("json", json),
            (false, true) => ("toml", toml),
            _ => panic!("{id}: expected exactly one manifest.json or manifest.toml"),
        };
        let manifest = manifest.canonicalize().unwrap();
        writeln!(
            code,
            "    ({id:?}, {format:?}, include_str!({:?}), &[",
            manifest.display()
        )
        .unwrap();
        for (name, path) in files {
            let path = path.canonicalize().unwrap();
            writeln!(
                code,
                "        ({name:?}, include_str!({:?})),",
                path.display()
            )
            .unwrap();
        }
        code.push_str("    ]),\n");
    }
    code.push(']');
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("bundled_plugins.rs");
    fs::write(out, code).unwrap();
}
