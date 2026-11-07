# Restart recovery

Active runs pause during graceful shutdown and automatically resume when the worker starts again. Unexpected process/container loss follows the same recovery path. The run ID, activity log, Codex conversation, workspace (including uncommitted changes), agent policy and selected skills survive. The browser retries failed activity requests automatically.

Recovery uses the account with the most available usage at the time of resumption. If no account is available, the run waits. Downtime does not consume the remaining execution budget. The worker checkpoints this budget every five seconds and during transitions; a hard crash can restore up to five seconds of budget since the last checkpoint. Explicitly choosing **Resume** gives the conversation a new timeout budget.

Explicit cancellations stay stopped. Failed and cancelled runs with retained history offer **Resume** in the run view. **Run again** starts a separate run with current task instructions. Runs that completed before shutdown are finalized from their checkpoint without starting another model turn. A missing checkpoint or lost retained session requires manual review.

## Process and container ownership

Chat recovery requests thread metadata without embedding its history, then pages turn metadata and individual items. It retains user message receipts (including messages steered into an existing turn) and the final assistant answer, discarding historical tool output after reading each page. Codex summary view alone is insufficient because it omits steered messages. This prevents long histories from overflowing a single RPC response and prevents accepted messages from being sent twice. Individual Codex frames remain bounded at 32 MB; MCP frames retain their separate 2 MB limit, with an explicit transport error if either limit is exceeded.

Each attempt records its own failure separately from the last successful result. A failed attempt displays that failure, never a previous reply or partial assistant commentary. Starting another attempt clears the error; dismissing the chat alert hides it until a new failure or page reload.

The worker records a supervisor's PID, Linux boot ID and process start time before allowing it to launch Codex. The supervisor watches its IPC connection and terminates its child process group when its manager disappears. Recovery verifies this identity and waits for the old process to stop before launching a replacement.

Isolated executions use unique attempt IDs. The broker serializes start/stop requests and retains stop markers in its dedicated `/runner-state` volume, rejecting late starts even after its own restart. Recovery requires confirmation that the old container was removed. If Docker or the broker is unavailable, the run remains queued with a recovery message; its project and account stay reserved. Credentials are recovered and removed only after the previous execution is fenced. Fresh MCP grants and scoped credentials are issued for the next attempt.

Prepared workspaces and private Codex homes remain under `/data/runs/<run-id>`. If shutdown interrupted workspace preparation, recovery creates a separate workspace generation instead of overwriting partially prepared files. Current agent access and saved paths are revalidated. A lost session or invalid workspace produces a visible failure rather than restarting the original prompt from scratch.

## Persistence and limits

Keep the existing `/data`, `/home/node` and project volumes, plus the broker’s `runner-state:/runner-state` volume from `compose.yaml`. The broker keeps read-only access to `/data`; only its stop-marker volume needs write access. Add this mount when upgrading an existing Coolify Compose service. Back up the database, vault key, run directories and project files together. Session history and workspaces can contain sensitive project material; protect backups accordingly. This is single-manager recovery for the existing Linux deployment, not shared-volume active/active orchestration.

Resume continues the model conversation, not the memory of a killed shell process. A command interrupted in the middle may need repair. The continuation prompt instructs the agent to verify previously completed external effects before repeating them; model-driven PR creation, releases and other external actions cannot be guaranteed exactly once. Worktree cleanup disables resume, and removing retained session files prevents conversation recovery.
