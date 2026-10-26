package build.cairn.app.data

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.encodeToJsonElement

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
    val updateRequired: Boolean = false,
)

const val DRAFT_AGENT_ID = "00000000-0000-4000-8000-000000000000"

const val MAIN_AGENT_ID = "00000000-0000-4000-8000-000000000001"

typealias AccessPolicy = build.cairn.contracts.AccessPolicy

typealias AgentPortrait = build.cairn.contracts.AgentAvatar

typealias Agent = build.cairn.contracts.Agent

typealias Project = build.cairn.contracts.Project

typealias Task = build.cairn.contracts.Mission

typealias ScheduleFailure = build.cairn.contracts.ScheduleFailure

fun Task.settings(): build.cairn.contracts.MissionSettings =
    build.cairn.contracts.MissionSettings(
        name = name,
        prompt = prompt,
        agentId = agentId,
        projectId = projectId,
        skills = skills,
        tags = tags,
        cron = cron,
        timezone = timezone,
        enabled = enabled,
        archived = archived,
        worktree = worktree,
    )

fun Task.runTaskSnapshot(): build.cairn.contracts.RunTaskSnapshot =
    wireJson.decodeFromJsonElement(
        build.cairn.contracts.RunTaskSnapshot.serializer(),
        wireJson.encodeToJsonElement(build.cairn.contracts.Mission.serializer(), this),
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
    val task: build.cairn.contracts.RunTaskSnapshot =
        build.cairn.contracts.RunTaskSnapshot(
            id = "00000000-0000-4000-8000-000000000000",
            name = "",
            prompt = "",
            agentId = MAIN_AGENT_ID,
            skills = null,
        ),
    val agent: Agent =
        Agent(
            exclusiveNodes = emptyList(),
            plugins = emptyList(),
            id = "00000000-0000-4000-8000-000000000000",
            name = "",
            revision = "",
            createdAt = 0,
            access =
                AccessPolicy(
                    mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                    projects = null,
                    skills = null,
                    mcps = null,
                ),
        ),
    val project: Project? = null,
    val projects: List<Project> = emptyList(),
    val skills: List<SnapshotSkill> = emptyList(),
)

@Serializable
data class RunRetryCause(val kind: String, val message: String, val startup: Boolean = false)

@Serializable
data class RunRetry(
    val attempt: Int,
    val limit: Int,
    val nextAttemptAt: Long? = null,
    val cause: RunRetryCause,
    val startedAt: Long? = null,
    val deadlineAt: Long? = null,
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
    val nativeMode: NativeModeState? = null,
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

typealias Occurrences = build.cairn.contracts.SchedulePreviewOutput

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
