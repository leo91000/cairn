# Plugin-owned program installation and JSON/TOML manifests

Date: 2026-10-18.

## Decision

Packages retain a declarative manifest, authored as exactly one `manifest.json` or
`manifest.toml`. Both parse into the same validated `Manifest`; uploads are persisted
as canonical JSON. The management interface and web/Android clients continue to use
JSON. Equivalent JSON and TOML have the same approval digest. Existing JSON packages
with explicit program paths retain their digest and behavior.

Each declared program specifies exactly one of `path` (an administrator-installed
absolute path) or `installer` (a package-relative Luau module). An installer exports
`resolve(input)` and `install(input)`; the latter receives `input.release`, the exact
result selected earlier. Both receive the server platform, never instance settings,
credentials or secrets. The installer module and its dependencies are reviewed with
all other package files. This supersedes ADR-0045's restriction that packages cannot
prepare executables; the manifest stays data as in ADR-0047.

Release discovery and distribution knowledge belong to the plugin. Cairn supplies
bounded, public HTTPS metadata requests, streamed downloads and executable extraction
through temporary handles. Only declared hosts are reachable, without redirects.
Resolution cannot install files or run programs. Installation can only download the
selected artifact and extract its selected executable; it cannot call instance
capabilities or execute an arbitrary installation command. Initial formats are ZIP
and an unarchived binary. Paths and links in ZIPs are checked; only one regular file
is materialized. Downloads are limited to 64 MiB and executables to 128 MiB.

Cairn records the exact release, archive hash and executable hash under
`<data>/plugin-programs/<plugin>/<package-digest>/<program>/<os>-<arch>/`. The lock
switch is atomic, and immutable executable files retain the previous installed
release for restoration. Before execution, Cairn verifies the executable hash. A
missing/corrupt locked file fails rather than silently selecting a newer release.
Uploaded versions prepare their programs after approval and before activation; a
failure preserves the prior active package. Bundled programs prepare on first use.
Cold one-shot calls have a 180-second enclosing budget, allowing the separate
120-second installer event and subsequent 30-second executable call to finish;
prepared calls retain the normal 35-second host deadline. Long-lived workers prepare
dependencies before dispatch. Metadata-only handlers do not eagerly install programs.
No download or network access occurs merely by uploading or listing a package.

Web and Android offer the same check, explicit install and restore actions.
Owner preparation requests return an operation ID immediately and clients poll its
result with short requests, including installer-backed package activation and cold
instance handlers that may execute a program. Saves without a form handler keep
their synchronous response and do not download dependencies. This
respects the existing 30-second relay deadline. Operations are bounded to four
concurrent jobs, a nine-minute total budget and 64 recent in-memory results. On a
server restart, refresh the authoritative package/program locks; there is no
implicit replay of a mutation or persistent operation migration. A
candidate digest binds installation to the release the owner saw. Checking again
can invalidate an earlier candidate. No periodic updater or automatic upgrade policy
is introduced in this change. Restarting always uses the installed lock. Changing
package version scopes program locks separately, so a package rollback retains its
own program release.

The 1Password package declares its official release/distribution hosts and supplies
the `op` installer for Linux x86_64/aarch64, accepting stable CLI >=2.39.0 <3. The
Docker image no longer installs `op`. The official ZIP source provides no SHA-256
sidecar: provenance relies on the approved official HTTPS host, and Cairn records
the downloaded hash. This is not verification of the bundled OpenPGP signature.
Other installers can supply a publisher SHA-256 that Cairn checks before extraction.
A moving latest URL is not used for execution after installation.
