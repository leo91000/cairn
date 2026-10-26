# Native execution behind coding-agent adapters

Date: 2026-10-12.

## Context

Accounts already use a shared pool with native authentication and usage drivers
([shared-account-pool decision](0061-one-account-pool-for-coding-agents.md)). Execution still made repeated Claude-versus-Codex choices in the worker,
runner entrypoint, recovery and MCP configuration. A third coding agent would have
expanded those choices throughout shared orchestration. Web and Android model
catalogs also stored one field for each of the two existing coding agents.

## Decision

Each `Provider` selects an `ExecutionAdapter` separately from its account `Driver`.
The adapter owns the native executable, private state directory and environment,
credential installation, launch settings, model discovery, session recovery and
protocol execution. Native protocol implementations continue translating their
output into the existing conversation events. The worker retains admission,
workspace ownership, cancellation, deadlines, account switching and fencing.

MCP authorization builds an engine-independent collection of HTTP and command
servers with their allowed tools. Only the selected adapter serializes that
collection into its native configuration. The gateway and run grants remain shared;
Codex receives allowlists and per-thread bearer headers, while Claude receives its
native MCP document and denied command-tool names. All components run the same version.

Explicit unknown or malformed coding-agent values fail rather than selecting Codex.
 Web and Android
store model catalogs by coding-agent identifier and use a metadata registry for
selection and labels. Existing model endpoints and saved model selections remain
unchanged.

Codex's resident process, prepared VM eligibility, schema templates and background
image/title generation remain explicit Codex features. This decision does not
require other coding agents to implement those optimizations or capabilities.

## Adding another coding agent

Register its identifier and implement the execution adapter. Add an account driver
for the authentication methods actually supported, without assuming a subscription
or readable usage quota. Register client metadata and install the pinned native CLI
in the runner image. A coding engine and its underlying model providers are distinct
concepts: adding OpenCode must preserve that distinction in model and credential
selection rather than treating every underlying provider as another coding agent.

This change adds the extension boundary; it does not implement OpenCode, credential
sharing between engines, or a runtime plugin loader. Adapter tests check native MCP
permissions, thread credentials, environment isolation and invalid selection. The
existing native CLI fixtures remain the reference for execution and resume behavior.
