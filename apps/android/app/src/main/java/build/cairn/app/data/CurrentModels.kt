package build.cairn.app.data

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement

@Serializable
data class ChatAttachment(
    val id: String,
    val chatId: String = "",
    val name: String,
    val size: Long = 0,
    val mediaType: String = "application/octet-stream",
    val kind: String = "file",
)

@Serializable
data class ChatMessage(
    val id: String,
    val chatId: String = "",
    val text: String = "",
    val model: String = "",
    val reasoning: String = "",
    val mode: String = "queue",
    val status: String = "queued",
    val createdAt: Long = 0,
    val attachments: List<ChatAttachment> = emptyList(),
    val questionId: String? = null,
    val provider: String = "",
)

@Serializable data class QuestionOption(val label: String, val description: String = "")

@Serializable
data class QuestionField(
    val id: String,
    val title: String,
    val secret: Boolean = false,
    val options: List<QuestionOption> = emptyList(),
)

@Serializable
data class ChatQuestion(
    val id: String,
    val chatId: String,
    val runId: String = "",
    val blocking: Boolean = false,
    val fields: List<QuestionField> = emptyList(),
    val status: String = "pending",
    val createdAt: Long = 0,
)

@Serializable
data class Chat(
    val id: String,
    val title: String = "Nouvelle conversation",
    val agentId: String = MAIN_AGENT_ID,
    val projectId: String? = null,
    val runId: String? = null,
    val paused: Boolean = false,
    val createdAt: Long = 0,
    val updatedAt: Long = 0,
    val pendingQuestions: Int = 0,
    val agentName: String = "",
    val projectName: String? = null,
    val status: String = "idle",
    val questions: List<ChatQuestion> = emptyList(),
    val run: Run? = null,
    val messages: List<ChatMessage> = emptyList(),
    val error: String? = null,
    val lifecycle: String = "active",
    val purgeAt: Long? = null,
    val restoredAt: Long? = null,
    val sessionRestartRequested: Boolean = false,
    val lifecycleError: String? = null,
)

@Serializable
data class Deliverable(
    val id: String,
    val runId: String,
    val messageId: String? = null,
    val key: String,
    val version: Int = 1,
    val title: String = "",
    val name: String = "",
    val group: String = "",
    val kind: String = "file",
    val mediaType: String = "application/octet-stream",
    val size: Long = 0,
    val createdAt: Long = 0,
    val visibility: String = "private",
    val publicUrl: String? = null,
    val previewStatus: String = "none",
    val excerpt: String? = null,
) {
    fun path(preview: Boolean = false) =
        "/runs/${segment(runId)}/artifacts/${segment(id)}" +
            if (preview) "?preview=1" else "?download=1"
}

fun latestArtifacts(items: List<Deliverable>) =
    items
        .groupBy { it.runId to it.key }
        .values
        .map { versions -> versions.maxBy { it.version } }
        .sortedBy { it.createdAt }

@Serializable
data class LiveState(
    val run: Run? = null,
    val chat: Chat? = null,
    val chats: List<Chat>? = null,
    val artifacts: List<Deliverable> = emptyList(),
    val cacheRevision: String? = null,
)

@Serializable
data class LiveBatch(
    val events: List<RunEvent>,
    val state: LiveState? = null,
    val reset: Boolean,
    val more: Boolean,
    val history: String? = null,
    val oldest: Long? = null,
    val hasOlder: Boolean = false,
)

@Serializable
data class HistoryPage(
    val events: List<RunEvent>,
    val history: String,
    val oldest: Long,
    val hasOlder: Boolean,
)

@Serializable data class ReasoningOption(val reasoningEffort: String, val description: String = "")

@Serializable
data class CodexModel(
    val model: String,
    val displayName: String = "",
    val description: String = "",
    val hidden: Boolean = false,
    val isDefault: Boolean = false,
    val defaultReasoningEffort: String = "",
    val supportedReasoningEfforts: List<ReasoningOption> = emptyList(),
)

@Serializable
data class ModelCatalog(
    val models: List<CodexModel> = emptyList(),
    val checkedAt: Long? = null,
    val stale: Boolean = false,
    val error: String = "",
)

@Serializable
data class McpTool(
    val name: String,
    val title: String? = null,
    val description: String? = null,
    val inputSchema: JsonElement? = null,
)

@Serializable
data class Mcp(
    val id: String = "",
    val name: String = "",
    val transport: String = "http",
    val url: String = "",
    val command: String = "",
    val args: List<String> = emptyList(),
    val auth: String = "none",
    val clientId: String = "",
    val scopes: String = "",
    val allowPrivateNetwork: Boolean = false,
    val enabled: Boolean = true,
    val enabledTools: List<String>? = null,
    val state: String = "untested",
    val error: String = "",
    val tools: List<McpTool> = emptyList(),
    val checkedAt: Long? = null,
    val hasToken: Boolean = false,
    val hasClientSecret: Boolean = false,
    val envKeys: List<String> = emptyList(),
    val callbackUrl: String = "",
)

@Serializable data class UrlResult(val url: String)

@Serializable data class Configured(val configured: Boolean = false)

@Serializable
data class RunWorkspace(
    val projectId: String,
    val path: String,
    val kind: String = "worktree",
    val revision: String? = null,
)

/**
 * Recognize only a delivered file on the connected origin; tool-provided URLs are never fetched.
 */
fun artifactForLink(
    link: String,
    origin: okhttp3.HttpUrl,
    artifacts: List<Deliverable>,
    installationId: String? = null,
): Deliverable? {
    val path = artifactPathForLink(link, origin, installationId) ?: return null
    return artifacts.find { path == it.path().substringBefore('?') }
}

/** Only canonical artifact endpoints on our authenticated server may use the session. */
fun artifactPathForLink(
    link: String,
    origin: okhttp3.HttpUrl,
    installationId: String? = null,
): String? {
    val url = origin.resolve(link) ?: return null
    if (
        url.scheme != origin.scheme ||
            url.host != origin.host ||
            url.port != origin.port ||
            url.username.isNotEmpty() ||
            url.password.isNotEmpty()
    )
        return null
    val prefix =
        if (installationId == null) "/api" else "/api/installations/${segment(installationId)}/api"
    val path =
        when {
            url.encodedPath.startsWith("$prefix/") -> url.encodedPath.removePrefix(prefix)
            // Agent messages may carry an installation-relative link.
            installationId != null && url.encodedPath.startsWith("/api/runs/") ->
                url.encodedPath.removePrefix("/api")
            else -> return null
        }
    return path.takeIf { Regex("/runs/[A-Za-z0-9_-]+/artifacts/[A-Za-z0-9_-]+").matches(it) }
}
