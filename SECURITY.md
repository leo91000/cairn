# Security model

Cairn is a private, single-owner application for trusted coding agents.
Administrator access can configure tasks, projects, and skills.
Every run uses Codex YOLO mode (`--dangerously-bypass-approvals-and-sandbox`).
Docker provides isolation; Codex has no inner sandbox and does not prompt for approval.
Runs execute as the container's non-root user and can access its files, mounted
volumes, credentials, and network. This is not a boundary between untrusted tenants.

Use HTTPS for remote deployments. Browser passwords are scrypt-hashed; sessions,
OAuth access/refresh tokens, and authorization codes are stored by token hash.
CSRF, Origin and Host checks protect browser routes; MCP requires audience-bound
bearer tokens and verifies scopes inside each tool. Sensitive auth headers and
password fields are redacted from application logging. CLI credential files remain
managed by their providers in the persistent worker home.

Artifacts are private by default. Explicit public sharing grants anonymous GET/HEAD
access to one immutable file version through an unguessable, revocable URL. Public
routes never expose artifact metadata, run listings, or conversations. File
responses retain sandbox CSP, nosniff and no-store headers. Public URLs are bearer
capabilities: anyone who receives one can download that file. Revoking blocks
future requests, not downloads already in progress or copies already saved.
Visibility changes require an owner session with CSRF protection or a live
agent grant scoped to that artifact’s run. New versions default to private.

Project registration and skill access resolve paths under configured roots.
Supporting files reject traversal and escaping symlinks. Worktree cleanup only
operates on the manager's recorded worktrees and refuses dirty/untracked content.
Agents can modify their runtime environment; use trusted instructions and do not
treat the application database as isolated from the worker account. Running outside
Docker grants the same access as the host user running the worker.

Do not put credentials into task prompts or skills. Common token forms are redacted
from captured output, but arbitrary secrets intentionally printed by a command
cannot be recognized reliably. Protect volume backups and retained run data.

For a suspected vulnerability, use this repository's GitHub private vulnerability
reporting when available. Otherwise contact the repository owner privately before
opening an issue containing sensitive details. Do not include live tokens, account
files, or private prompts in reports.
