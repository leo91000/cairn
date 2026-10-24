# Architecture decisions

Each decision is identified by its **full filename**, linked with its subject.
The numeric prefix is historical: several decisions were introduced concurrently
with the same number. A bare reference such as “ADR-0037” does not distinguish
those decisions.

Existing filenames remain stable so links from issues, PRs and older documents
keep working. Use a descriptive link when citing a decision, for example
[durable conversation wakeups](0037-durable-conversation-wakeups.md).

## Historical number collisions

| Prefix | Distinct decisions |
|---|---|
| 0002 | [Conversation movement by pause/resume](0002-conversation-movement-by-pause-and-resume.md) · [One account pool for coding agents](0002-one-account-pool-for-coding-agents.md) |
| 0012 | [Shared node budgets](0012-shared-node-budgets.md) · [Verified block cache and active backup budget](0012-verified-block-cache-and-active-backup-budget.md) |
| 0032 | [Bounded agent-error recovery](0032-bounded-agent-error-recovery.md) · [Single official relay process](0032-single-official-relay-process.md) |
| 0036 | [Compressed immutable disk objects](0036-compressed-immutable-disk-objects.md) · [Opt-in installation tools for agents](0036-opt-in-installation-tools-for-agents.md) |
| 0037 | [Coding-agent execution adapters](0037-coding-agent-execution-adapters.md) · [Durable conversation wakeups](0037-durable-conversation-wakeups.md) · [Readable workspace URLs](0037-readable-workspace-urls.md) |
| 0038 | [Durable condition waits](0038-durable-condition-waits.md) · [Luau installation plugins and external delegation](0038-luau-installation-plugins-and-external-bot-delegation.md) |

| 0051 | [Agent-centric permissions](0051-agent-centric-permissions.md) · [Transient plugin contexts](0051-transient-plugin-contexts.md) |

The other decisions can be found by their descriptive filenames in this directory.
This index disambiguates references; it does not change any decision or supersession.
