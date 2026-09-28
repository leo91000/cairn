package dev.leo.manager.data

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement

@Serializable
data class Session(
    val authenticated: Boolean = false,
    val csrf: String = "",
    val setupRequired: Boolean = false,
)

const val MAIN_AGENT_ID = "00000000-0000-4000-8000-000000000001"

@Serializable
data class AccessPolicy(
    val nodes: List<String>? = listOf(LOCAL_NODE_ID),
    /** Largest resources the agent may request per conversation; null leaves only node ceilings. */
    val maxResources: NodeResources? = null,
    val projects: List<String>? = null,
    val skills: List<String>? = null,
    val mcps: List<String>? = null,
    val mcpTools: Map<String, List<String>> = emptyMap(),
    val github: Boolean = true,
    val sandbox: String = "yolo",
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
    val provider: String = "codex",
    val avatar: AgentPortrait? = null,
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
)

@Serializable
data class SnapshotSkill(val name: String, val path: String = "", val content: String = "")

@Serializable
data class Snapshot(
    val task: Task = Task(),
    val agent: Agent = Agent(),
    val project: Project? = null,
    val projects: List<Project> = emptyList(),
    val skills: List<SnapshotSkill> = emptyList(),
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
    val usage: Map<String, JsonElement>? = null,
    val recoveryPending: Boolean = false,
    val resumeAvailable: Boolean = false,
    val accountName: String? = null,
    /** The coding agent whose accounts need the user in Connexions before this run can start. */
    val accountRequired: String? = null,
    val accountWaitReason: String? = null,
    val cancelRequestedAt: Long? = null,
    val resumeCount: Int = 0,
    val workspaces: List<RunWorkspace> = emptyList(),
    val isolated: Boolean = false,
    val outcome: TaskOutcome? = null,
    val chatExecution: ChatExecution? = null,
    val error: String? = null,
    val nodeId: String? = null,
    val nodeState: String? = null,
    val resources: NodeResources? = null,
    val pinnedNodeId: String? = null,
    val preferredNodeId: String? = null,
    val capacityWaitUntil: Long? = null,
    val restoredAt: Long? = null,
    val movementError: String? = null,
    val backup: NodeBackup? = null,
) {
    val active
        get() = status == "running" || status == "queued"

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
data class Connection(
    val provider: String,
    val installed: Boolean = false,
    val connected: Boolean = false,
    val account: String? = null,
    val version: String? = null,
)

@Serializable
data class DeviceFlow(
    val provider: String = "",
    val state: String = "",
    val code: String? = null,
    val url: String? = null,
    val error: String? = null,
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
)

@Serializable
data class ChatExecution(
    val messageId: String,
    val text: String = "",
    val recovery: Boolean = false,
)

@kotlinx.serialization.Serializable
data class GithubRepository(
    val fullName: String,
    val name: String,
    val description: String = "",
    val defaultBranch: String = "",
    @kotlinx.serialization.SerialName("private") val isPrivate: Boolean = false,
    val archived: Boolean = false,
    val fork: Boolean = false,
    val owner: String = "",
    val language: String = "",
    val stars: Int = 0,
    val pushedAt: String = "",
    val imported: Boolean = false,
)

@kotlinx.serialization.Serializable
data class GithubRepositoryPage(val repositories: List<GithubRepository>, val nextPage: Int? = null)
