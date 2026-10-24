package build.cairn.app.data

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement

@Serializable
data class Session(
    val authenticated: Boolean = false,
    val csrf: String? = null,
    val account: CairnAccount? = null,
    val installations: List<Installation> = emptyList(),
)

@Serializable data class EmailChallenge(val challenge: String)

@Serializable data class CairnAccount(val id: String, val email: String)

@Serializable
enum class InstallationRole(val value: String, val label: String) {
    @SerialName("owner") Owner("owner", "Propriétaire"),
    @SerialName("member") Member("member", "Membre"),
}

@Serializable
data class Installation(
    val id: String,
    val name: String,
    val role: InstallationRole = InstallationRole.Member,
    val online: Boolean = false,
)

const val MAIN_AGENT_ID = "00000000-0000-4000-8000-000000000001"

@Serializable
data class AccessPolicy(
    /** Nodes its conversations may run on; null means every node, including future ones. */
    val nodes: List<String>? = listOf(LOCAL_NODE_ID),
    /** Projects the agent may open; null means every project, including future ones. */
    val projects: List<String>? = null,
    val skills: List<String>? = null,
    val mcps: List<String>? = null,
    val mcpTools: Map<String, List<String>> = emptyMap(),
    val installation: Boolean = false,
    /**
     * Hands tasks to conversations that report back, on any coding agent and model. Null when the
     * installation never stored it: left out of saves, so an older installation accepts them.
     */
    val delegation: Boolean? = null,
    val sandbox: String = "yolo",
    val backend: String = "firecracker",
)

@Serializable
data class AgentPortrait(
    val status: String = "ready",
    val revision: String = "",
    val url: String? = null,
    val error: String? = null,
)

@Serializable
data class Agent(
    val id: String = "",
    val name: String = "",
    val description: String = "",
    val model: String = "",
    val reasoning: String = "high",
    val instructions: String = "",
    val timeoutMinutes: Int = 0,
    val access: AccessPolicy = AccessPolicy(),
    /**
     * Nodes on which the agent may suspend every other conversation during intensive work. Only
     * effective where it may also run. A live grant: it applies immediately, even in running
     * conversations. Kept outside [access] so it never enters a run's frozen policy.
     */
    val exclusiveNodes: List<String> = emptyList(),
    /**
     * Plugin instances (e.g. 1Password service accounts) whose commands and tools it gets. A live
     * grant, kept outside [access] like [exclusiveNodes].
     */
    val plugins: List<String> = emptyList(),
    val provider: String = "codex",
    val avatar: AgentPortrait? = null,
    /**
     * Set by installations that store live grants on the agent (computed, never stored, never
     * sent). Absent on earlier or rolled-back releases, which keep them on the resources.
     */
    val grantsOnAgent: Boolean = false,
    /** Changes on every save; sent back so a stale editor cannot overwrite newer access. */
    val revision: String = "",
)

@Serializable
data class Project(
    val id: String = "",
    val name: String = "",
    val path: String = "",
    val description: String = "",
    val baseBranch: String = "main",
    val origin: String? = null,
    val sourceMode: String = "remote",
)

@Serializable
data class Task(
    val id: String = "",
    val name: String = "",
    val prompt: String = "",
    val agentId: String = "",
    val projectId: String? = null,
    val skills: List<String>? = null,
    val tags: List<String> = emptyList(),
    val cron: String? = null,
    val timezone: String = "Europe/Paris",
    val enabled: Boolean = true,
    val archived: Boolean = false,
    val worktree: Boolean = true,
    val nextRun: Long? = null,
)

@Serializable
data class Skill(
    val name: String = "",
    val description: String = "",
    val scope: String = "global",
    val content: String = "",
    val path: String = "",
    val valid: Boolean = true,
    val error: String? = null,
    /** Counts over the whole skill directory; see docs/SKILLS.md. */
    val files: Int = 0,
    val bytes: Long = 0,
    val executables: Int = 0,
    val source: SkillSource? = null,
)

@Serializable
data class SnapshotSkill(
    val name: String,
    val path: String = "",
    val content: String = "",
    val bundle: String? = null,
)

@Serializable
data class Snapshot(
    val task: Task = Task(),
    val agent: Agent = Agent(),
    val project: Project? = null,
    val projects: List<Project> = emptyList(),
    val skills: List<SnapshotSkill> = emptyList(),
)

@Serializable data class RunRetryCause(val kind: String, val message: String)

@Serializable
data class RunRetry(
    val attempt: Int,
    val limit: Int,
    val nextAttemptAt: Long? = null,
    val cause: RunRetryCause,
)

@Serializable
data class Run(
    val id: String,
    val taskId: String = "",
    val projectId: String? = null,
    val taskName: String = "",
    val status: String,
    val trigger: String = "",
    val createdAt: Long = 0,
    val startedAt: Long? = null,
    val finishedAt: Long? = null,
    val summary: String = "",
    val sessionId: String? = null,
    val workspace: String? = null,
    val workspaceCleanedAt: Long? = null,
    val snapshot: Snapshot = Snapshot(),
    /** The run's coding agent, as run lists report it without the snapshot. */
    val provider: String? = null,
    val usage: Map<String, JsonElement>? = null,
    val recoveryPending: Boolean = false,
    val resumeAvailable: Boolean = false,
    val accountName: String? = null,
    /** The coding agent whose accounts need the user in Connexions before this run can start. */
    val accountRequired: String? = null,
    val accountWaitReason: String? = null,
    val retry: RunRetry? = null,
    val cancelRequestedAt: Long? = null,
    val resumeCount: Int = 0,
    val workspaces: List<RunWorkspace> = emptyList(),
    val isolated: Boolean = false,
    val outcome: TaskOutcome? = null,
    /** The conversation or mission whose agent started this run. */
    val requestedBy: RunRequester? = null,
    /** This account set the run's outcome aside: dismissed, or snoozed until a time. */
    val dismissal: OutcomeDismissal? = null,
    val chatExecution: ChatExecution? = null,
    val error: String? = null,
    /** A mission run that finished after this account last opened its mission. */
    val unread: Boolean = false,
    /** The opening of the agent's final message, while unread. */
    val preview: String? = null,
    val nodeId: String? = null,
    val nodeState: String? = null,
    val resources: NodeResources? = null,
    val pinnedNodeId: String? = null,
    val preferredNodeId: String? = null,
    val capacityWaitUntil: Long? = null,
    val restoredAt: Long? = null,
    val movementError: String? = null,
    val backup: NodeBackup? = null,
    val storage: RunStorage? = null,
    val suspendedByExclusive: ExclusiveReservation? = null,
    val wakeup: ChatWakeup? = null,
    val executionBackend: String? = null,
) {
    val active
        get() = status in listOf("running", "queued", "waiting")

    val title
        get() = taskName.ifBlank { snapshot.task.name }.ifBlank { "Exécution" }
}

@Serializable
data class RunEvent(
    val id: Long,
    val createdAt: Long,
    val type: String,
    val text: String,
    val payload: Map<String, JsonElement>? = null,
    // Presentation identity survives newer wire event IDs and encrypted cache restores.
    val displayId: Long? = null,
    // A conversation streams several runs; item IDs such as a subagent's are scoped to their run.
    val runId: String = "",
)

@Serializable
data class Overview(
    val counts: Map<String, Int> = emptyMap(),
    val agents: Int = 0,
    val projects: Int = 0,
    val tasks: List<Task> = emptyList(),
    val runs: List<Run> = emptyList(),
    val concurrency: Int = 1,
)

@Serializable
data class Settings(
    val nativeMcpOauth: Boolean = false,
    val publicUrl: String = "",
    val workspaceRoots: List<String> = emptyList(),
    val home: String = "",
    val concurrency: Int = 1,
    val mcpUrl: String = "",
    val version: String = "",
    val commit: String = "",
    val protocol: String = "",
)

@Serializable
data class Grant(
    val id: String,
    val label: String = "",
    val scopes: List<String> = emptyList(),
    val clientId: String = "",
)

@Serializable
data class Audit(
    val id: Long,
    @kotlinx.serialization.SerialName("created_at") val at: Long = 0,
    val action: String = "",
    val detail: String = "",
)

@Serializable data class Occurrences(val occurrences: List<Long>)

@Serializable data class FileContent(val content: String)

@Serializable data class TokenResult(val token: String)

@Serializable data class OAuthClient(val client_name: String = "")

@Serializable data class ConsentPreview(val client: OAuthClient, val scopes: List<String>)

@Serializable data class ConsentResult(val redirect: String)

@Serializable
data class TaskOutcome(
    val status: String,
    val reason: String = "",
    val evidence: List<String> = emptyList(),
    val reportedAt: Long = 0,
    val messageId: String? = null,
    /** Who must act next on blocked work: user (the default), requester or external. */
    val waitingOn: String? = null,
)

@Serializable
data class RunRequester(val runId: String, val chatId: String? = null, val taskId: String? = null)

/** A dismissed outcome has no [until]; a snoozed one asks again then. */
@Serializable data class OutcomeDismissal(val until: Long? = null)

@Serializable
data class ChatExecution(
    val messageId: String,
    val text: String = "",
    val recovery: Boolean = false,
)
