package build.cairn.app.data

import kotlinx.serialization.Serializable

typealias ChatAttachment = build.cairn.contracts.ChatAttachment

typealias ChatMessage = build.cairn.contracts.ChatMessage

typealias QuestionOption = build.cairn.contracts.QuestionOption

typealias QuestionField = build.cairn.contracts.QuestionField

typealias QuestionAnswer = build.cairn.contracts.QuestionAnswer

typealias ChatQuestion = build.cairn.contracts.ChatQuestion

typealias ChatWakeup = build.cairn.contracts.ChatWakeup

typealias ContextUsage = build.cairn.contracts.ContextUsage

typealias ChatCompaction = build.cairn.contracts.ChatCompaction

typealias ChatRequester = build.cairn.contracts.ChatRequester

typealias CodingAgentSwitch = build.cairn.contracts.CodingAgentSwitch

typealias CodingAgentChoice = build.cairn.contracts.CodingAgentChoice

typealias CodingAgentOptions = build.cairn.contracts.CodingAgentOptions

typealias Chat = build.cairn.contracts.Chat

val Chat.exclusiveSuspended: Boolean
    get() =
        (run?.status ?: status) in listOf("running", "queued") &&
            (suspendedByExclusive != null || run?.suspendedByExclusive != null)

typealias Deliverable = build.cairn.contracts.Deliverable

fun Deliverable.path(preview: Boolean = false) =
    "/runs/${segment(runId)}/artifacts/${segment(id)}" +
        if (preview) "?preview=1" else "?download=1"

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

typealias HistoryPage = build.cairn.contracts.HistoryPage

typealias ReasoningOption = build.cairn.contracts.ReasoningOption

typealias ServiceTier = build.cairn.contracts.ServiceTier

typealias ModelRoute = build.cairn.contracts.ModelRoute

typealias CodingAgentModel = build.cairn.contracts.CodingAgentModel

typealias ModelCatalog = build.cairn.contracts.ModelCatalog

fun CodingAgentModel.route(value: String): ModelRoute? = routes.find { it.model == value }

/** Whether [value] selects this model, on any of its routes. */
fun CodingAgentModel.matches(value: String) = model == value || route(value) != null

typealias Mcp = build.cairn.contracts.McpView

typealias McpTool = build.cairn.contracts.McpTool

fun Mcp.settings(
    token: String? = null,
    clientSecret: String? = null,
    env: Map<String, String>? = null,
    removeEnv: List<String> = emptyList(),
) =
    build.cairn.contracts.McpInput(
        name = name,
        transport = transport,
        url = url,
        command = command,
        args = args,
        auth = auth,
        clientId = clientId,
        scopes = scopes,
        allowPrivateNetwork = allowPrivateNetwork,
        enabled = enabled,
        enabledTools = enabledTools,
        token = token,
        clientSecret = clientSecret,
        env = env?.let { build.cairn.contracts.McpEnvironment(it) },
        removeEnv = removeEnv,
    )

@Serializable data class UrlResult(val url: String)

typealias RunWorkspace = build.cairn.contracts.RunWorkspace

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

fun build.cairn.contracts.McpInput.replacement() =
    build.cairn.contracts.McpReplacementInput(
        name = name,
        transport = transport,
        url = url,
        command = command,
        args = args,
        auth = auth,
        clientId = clientId,
        scopes = scopes,
        allowPrivateNetwork = allowPrivateNetwork,
        enabled = enabled,
        enabledTools = enabledTools,
        token = token,
        clientSecret = clientSecret,
        env = env,
        removeEnv = removeEnv,
    )
