# Agents, task scope, and execution

Tasks choose an agent. **Main agent** is created automatically and includes all
registered projects, available skills, and shared connections, including future
additions. Its model, instructions, timeout, and execution mode are editable;
its resource access cannot be reduced and it cannot be deleted.
1Password service accounts, other plugin instances and exclusive node
reservations are explicit exceptions: even Main needs a grant in its **Secrets &
plugins** and **Nodes** sections. Those grants apply live; see
[1Password access](ONEPASSWORD.md) and [agent-centric permissions](adr/0051-agent-centric-permissions.md).

Every permission is edited on the agent's page, on web and Android. It summarises
effective access, opens one section per area, keeps a single draft and reviews
sensitive changes (new powers, secrets, reductions that stop work, suspended plugin
delegations) before saving. Nodes and plugin instances only list their agents.

**Allow access to the current Cairn installation** is also opt-in, including for
Main and existing agents. The owner enables it in the web or Android agent editor.
It automatically supplies `cairn_installation` to Codex and Claude Code, without
a server URL, personal access token, or OAuth setup. This delegates the complete
public management catalogue across the installation: agents, projects, global
and project skills, MCP connections, missions and runs. This authority applies
even when the agent's direct resource, plugin or node permissions are restricted.
It includes changing other agents' permissions and managing their work. Account
administration remains outside the public MCP catalogue.

**Task delegation** is a narrower opt-in power in the same section. It gives a
conversation's agent the `cairn_workspace` tools `delegate_task`, `task_status` and
`task_cancel`, without installation management. A delegated task is an ordinary
conversation, possibly on another coding agent and model, that records its
requester and reports back; the requester waits with the `task_finished`
condition of `wait_for`. The task runs as the requesting agent or as another agent
whose access the requester covers, in its own workspace. Delegated conversations
cannot delegate further and a requester has at most five working tasks. Turning
the power off is a reduction. See [delegated tasks](adr/0059-delegated-tasks.md).

The `delete_agent` management tool removes an unused agent, including its saved
portrait and plugin grants (GitHub accounts, 1Password). Main, agents referenced by tasks
(including archived tasks), and agents with active work cannot be deleted. Move
their tasks to another agent and finish or cancel active work first. Completed
run history is preserved.

New missions default to the calling agent when no agent ID is supplied; another
agent can be selected explicitly. A mission uses its selected agent's permissions,
plugin grants such as GitHub accounts and 1Password. Installation access can also inspect,
cancel or wait for runs belonging to other agents in the installation. The owner
delegates these operations through the agent setting; newly created scheduled
missions use the owner's authorship, as with the existing management MCP. The
ordinary author reconciliation still ends them if that owner's access ends.
Disabling installation access denies active calls immediately. Completed,
cancelled or superseded chat turns cannot reuse their token. Tokens issued
without installation access cannot acquire it by enabling the setting; a new
run or chat turn receives the updated setting. Older profile editors preserve
it when omitted.

Other agents can select projects, skills, MCP connections and tools. GitHub accounts
are plugin instances granted on the agent page, like 1Password. Agents created on the web or Android page start restricted (no
projects, skills, MCP connections or plugins, the current runner only) and the
owner widens them; the API still defaults omitted settings to full access and YOLO.
Restricted agents need the isolated runner, which every production installation has.
A task inherits its
agent's resources, or can choose its initial project context and skills under
**Customize task scope**. Task overrides cannot expand agent access. Project
restrictions are configured on the agent; selecting a task project does not revoke
access to its other authorized projects. A task needs no project;
it starts in a scratch workspace and opens repositories only when needed through
`open_project`. Selecting a project prepares only that repository at startup. Runs snapshot the effective resources when
queued. Reducing an agent's access (fewer projects, skills, MCP servers or tools,
or a stricter sandbox) prevents its queued runs, resumptions and chat
follow-ups from executing under the old grants, and revokes their project and MCP
access; enqueue again or start a new chat to use the new policy. Unchanged or wider
access, other agent settings, settings older versions saved and node grants do not
interrupt work: it continues with the grants it was queued with. Running work keeps
its snapshot: cancel it before changing permissions when immediate revocation matters.

Existing tasks retain their project, explicit skills, schedules, and history.
Existing agents retain unrestricted YOLO behavior. Database schema 3 is required;
back up before upgrading from 0.1.x. Restore the matching schema-2 backup as well as
the old image if rolling back.

## Execution modes

Every production run, including Main, executes in a Firecracker microVM. Resource
scope and Codex sandbox policy are independent of VM isolation.

| Mode | Project writes | Guest privileges |
| --- | --- | --- |
| YOLO (default) | Allowed in private copies | User 1000 with passwordless sudo; Docker available |
| Workspace write | Codex writable roots | User 1000 without sudo; Codex sandbox |
| Read only | Read-only workspace mounts | User 1000 without sudo; Codex sandbox |

The runner fails closed if KVM, the guest image, or isolation setup is unavailable.
Local development without RUNNER_URL retains subprocess execution; production
workers require RUNNER_URL.

Projects are independent Git clones inside the run's private disk. Agents publish
changes through Git rather than editing the manager's checkout. Existing
worktree=false tasks also use private copies in production. The same disk is
retained for subsequent chat turns, account handoffs and restart recovery.
Historical conversations import their saved files and session history once.

Only selected skill resources are copied; symbolic links in skills are rejected.
Project Codex configuration and skill discovery directories are masked. Configured
MCPs use the existing scoped gateway. Project contents may themselves contain
secrets; project selection does not remove credentials already stored there.

Account refresh tokens stay in the manager. A run-specific vsock relay exposes
only the selected account's access-token exchange. Chat steering, answers and
attachments are delivered to the guest while the agent runs. They do not require
host filesystem mounts or expose the manager's Docker socket.

## GitHub accounts

GitHub is the bundled GitHub plugin ([plugins](PLUGINS.md#github)). Each account
is an instance, connected by signing in with GitHub or with a pasted token, and
granted to agents on their page; nothing is granted by default, Main included. An
agent holds one GitHub account: its runs receive that account's credentials for Git
and the `gh` CLI. Remote authorization is determined by the account or token:
selecting projects here does not reduce its GitHub permissions, so give restricted
agents an account with a fine-grained token limited to the intended repositories.
Tokens are encrypted in the installation's vault, never returned by the API, and
written only into granted agents' run homes, which lose them when the grant is
withdrawn or the run ends. Plugin grants are live: withdrawing one does not stop
queued work, and a pending GitHub wait ends as unavailable.

## Git commit identity

Isolated runs keep the manager account's global Git `user.name` and `user.email`
when both are configured. Only these identity values are copied into the run.
Otherwise, runs of an agent granted a plugin account with a commit identity use it:
for GitHub, the account name (or login) and its
[GitHub noreply address](https://docs.github.com/en/account-and-profile/reference/email-addresses-reference).
Both commit author and committer use this identity, regardless of the agent name.
If neither identity is available, Git requires an explicit identity before committing;
it never substitutes the agent name or `agent@localhost`.
This applies when a run environment is prepared; existing commits and already
running environments are unchanged.

## Runner deployment

Use both services in compose.yaml with the same immutable CAIRN_IMAGE. The runner
needs Linux x86-64, hardware virtualization, /dev/kvm and /dev/net/tun. It owns
persistent runner-state storage and launches Firecracker through its jailer.
Its infrastructure capabilities stay outside the guest. The agent never receives
the runner credential, host devices or a host Docker socket.

Do not publish runner port 4311. The manager authenticates requests with the
private runner-secret file. Guest Internet access permits TCP on all ports to
public hosts, including SSH on custom ports, and DNS (UDP 53). Other UDP, private
networks, metadata endpoints, neighboring VMs and runner services remain blocked.
Remote Git operations can use HTTPS or SSH to public hosts. New inbound connections
to guests remain blocked.

See [MicroVM architecture and operations](MICROVMS.md) for storage, upgrades,
recovery, host prerequisites and validation.
