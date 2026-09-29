# Agent toolkit

The image provides a broad default toolbox to every agent. The manager service
keeps its image-pinned Node and pnpm runtime. Agent commands use mise's global
configuration at `/etc/mise/config.toml`, with project overrides resolved for each
command, including after changing directories in a noninteractive shell.

## Included tools

| Purpose | Tools |
| --- | --- |
| Runtimes and package managers | mise, Node LTS, npm bundled with Node, pnpm 12, Python 3, uv, Rust/Cargo/rustup, Go |
| Search and code inspection | ripgrep (`rg`), fd, ast-grep, bat, delta, tree, file |
| Structured data | jq, Mike Farah's yq, sqlite3, PostgreSQL client (`psql`) |
| Validation and automation | shellcheck, shfmt, actionlint, ruff, just, hyperfine |
| Building | GCC/G++, make, CMake, Ninja, pkg-config, OpenSSL/libffi development headers |
| Git and network | git, git-lfs, gh, OpenSSH client, curl, wget, dig, ip, ping, nc |
| Files and archives | zip/unzip, tar, xz, zstd, bzip2, rsync, patch, diff, less |
| Diagnostics | ps, lsof, strace (subject to the agent's sandbox) |
| Documents and media | Poppler (`pdftotext`, `pdftoppm`), ImageMagick, ffmpeg |
| Coding and browsers | Codex; existing Chromium, Firefox and WebKit provisioning |

`deploy/toolkit/tools.json` defines the global update channels. Its sibling
`versions.json` pins the initial build. The installer generates the global mise
config from those exact versions and installs shared tools into the image.
Codex and gh retain their existing dedicated, versioned installers and updater.
OS utilities come from Debian's supported package repositories.

## Project versions

Agents receive mise shims on PATH in ordinary and login shells. Discovery is enabled
for `.nvmrc`, `.node-version`, `.python-version`, `rust-toolchain.toml`, supported
Go version files and `package.json` package-manager pins, alongside `mise.toml` and
`.tool-versions`. More specific mise configuration takes precedence. npm follows
Node's bundled version unless the project explicitly configures npm in mise.

Use `mise exec -- <command>` when a command needs a project's mise environment
variables as well as its tools. Use `uv venv`, `uv sync` and `uv run` for Python
projects. Project version pins are never rewritten by the container updater.

The runner prepares user-level mise registration and shims before starting Codex.
Preinstalled runtimes are linked read-only into each home; additional versions and
package caches stay writable in that home. This also avoids mise's shared-install
path being selected for new versions discovered through package.json.
Rust receives a private rustup configuration and links to the image's toolchains.

Main-agent downloads persist in its existing home volume. Restricted agents get
fresh private homes, removed after the run; the immutable default tools are reused
without copying their binaries. No extra host mounts, credentials, Docker access,
or sandbox write permissions are granted. Restricted agents can use preinstalled
tools; installing a missing version inside a restricted Codex sandbox can be denied and
must be reported instead of bypassing that policy.

## Automatic updates

The existing VPS timer dispatches `cli-updates.yaml` daily. In addition to Codex and
gh, it resolves newer stable mise and global tool versions using the catalogue
inside the deployed image. It never downgrades versions. If everything is current,
it skips rebuilding, except for a weekly refresh of Debian packages.

Candidates use the original deployed application image and an exact version plan.
CI supplies its GitHub token only to the temporary resolver/test container and
through a BuildKit secret for downloads; it is not baked into images or passed to
production agents. The build verifies mise's release checksum, installs tools via mise's backends and
refreshes OS packages. It never updates itself inside a running agent process.
Container tests exercise every managed tool, real Rust compilation, a Python venv,
project version selection, packageManager pins, and all three browser engines.
Runner tests exercise the toolkit inside actual workspace-write/read-only sandboxes.
The deployment lease, concurrent-release check, health verification and
rollback apply to the entire toolbox update. See [CLI updates](CLI-UPDATES.md).

After changing the catalogue or runner integration, deploy a new application image
once; subsequent tool updates need no application version release. The health
endpoint reports the image's exact toolkit manifest and build time.

References: [mise system installs](https://mise.jdx.dev/mise-cookbook/docker.html),
[configuration](https://mise.jdx.dev/configuration.html),
[noninteractive shims](https://mise.jdx.dev/dev-tools/shims.html),
[Python](https://mise.jdx.dev/lang/python.html), [Rust](https://mise.jdx.dev/lang/rust.html).
