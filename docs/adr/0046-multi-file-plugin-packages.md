# Multi-file plugin packages

Date: 2026-10-16.

## Decision

A plugin package may split its Luau into modules. `main.luau` stays the entry point
and returns the handler table; other `.luau` files are loaded with
`require("./path")`, resolved relative to the requiring file like Luau's
require-by-string, to `path.luau` or `path/init.luau` inside the package.

- Workers receive every file in their `Load` frame and resolve `require` from that set
  only. The disk is never read, and the global `require` provided by the Luau runtime
  is removed. Each file runs once per worker in its own environment over the frozen
  globals; cycles and paths above the package are refused.
- Approval covers the manifest and every Luau file, by path, so an edited, added or
  linked file withdraws the approval. Packages approved before this change are
  reviewed again; there is no compatibility path for older digests or API v1.
- Limits: lowercase path segments, four levels, 64 modules and 384 KB of Luau per
  package, which keeps load frames under the 1 MB frame limit.
- Bundled packages are embedded by a build script that includes every file of
  `plugins/<id>/`. Uploads accept the files or the whole folder; reviews list the files
  and their sizes on the web and Android.

The Discord package is split into its Gateway session, its setup (form, test, view),
its conversation handlers and small libraries (REST, text, permissions).

## Consequences

The manifest stays a single file: it is what the owner approves capability by
capability. Packages cannot share modules with each other or load modules from Cairn;
a shared library would need signed packages first. Each worker of an instance loads
its own copy of the modules, so module-level state is per worker, as it already was
for a single file.

Builds on ADR-0044 and ADR-0045.
