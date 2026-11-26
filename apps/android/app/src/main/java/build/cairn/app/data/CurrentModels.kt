package build.cairn.app.data

import kotlinx.serialization.Serializable

typealias ChatAttachment = build.cairn.contracts.ChatAttachment

typealias ChatMessage = build.cairn.contracts.ChatMessage

typealias QuestionOption = build.cairn.contracts.QuestionOption

typealias QuestionField = build.cairn.contracts.QuestionField

typealias QuestionAnswer = build.cairn.contracts.QuestionAnswer

typealias ChatQuestion = build.cairn.contracts.ChatQuestion

typealias ChatSideQuestion = build.cairn.contracts.ChatSideQuestion

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
    val planTurnsEnabled: Boolean = false,
    val run: Run? = null,
    val chat: Chat? = null,
    val chats: List<Chat>? = null,
    val artifacts: List<Deliverable> = emptyList(),
    val cacheRevision: String? = null,
)

/** The conversation's run is the state's own `run`: compact streams and saved copies omit it. */
fun LiveState.withChatRun(): LiveState =
    if (chat == null || chat.run != null || run == null) this else copy(chat = chat.copy(run = run))

/** List changes on a compact stream (`compact=1`), applied to the list it sent first. */
@Serializable
data class ChatChanges(
    /** New conversations and conversations whose summary changed, in full. */
    val upserts: List<Chat> = emptyList(),
    /** Conversations that left the list. */
    val removed: List<String> = emptyList(),
    /** Every listed id in order, when the order changed beyond the removals. */
    val order: List<String>? = null,
)

fun List<Chat>.applyChanges(changes: ChatChanges): List<Chat> {
    val removed = changes.removed.toSet()
    val upserts = changes.upserts.associateBy { it.id }
    val kept = filter { it.id !in removed }
    val known = kept.associateBy { it.id }
    val order =
        changes.order
            ?: (changes.upserts.map { it.id }.filter { it !in known } + kept.map { it.id })
    return order.mapNotNull { upserts[it] ?: known[it] }
}

/**
 * The state after one stream batch. A compact batch sends its state without an unchanged list, and
 * list changes on their own; both apply to the list received before.
 */
fun nextLiveState(previous: LiveState?, state: LiveState?, changes: ChatChanges?): LiveState? {
    var next = state?.withChatRun() ?: previous
    if (next != null && state != null && state.chats == null && previous?.chats != null)
        next = next.copy(chats = previous.chats)
    val chats = next?.chats
    if (next != null && chats != null && changes != null)
        next = next.copy(chats = chats.applyChanges(changes))
    return next
}

/**
 * The saved stream paths of conversations that left the list: deleted, or no longer shared. Their
 * saved copies go, as on the web, with every saved run log (`HistoryCache.removeRunLogs`).
 */
fun leftChatPaths(previous: List<Chat>?, chats: List<Chat>?): List<String> {
    if (previous == null || chats == null) return emptyList()
    val listed = chats.mapTo(HashSet()) { it.id }
    // A list read moments earlier may not hold a conversation created since: only a conversation
    // older than the newest one listed, by the server's clock, is gone, as on the web.
    val newest = (previous + chats).maxOfOrNull { it.createdAt } ?: 0
    return previous
        .filter { it.id !in listed && it.createdAt < newest - RECENT_CHAT_MILLIS }
        .map { savedChatPath(it.id) }
}

/** The server shares a list for up to a quarter second; this leaves ample margin. */
private const val RECENT_CHAT_MILLIS = 10_000L

/** Where a conversation opened by its identifier is saved. */
fun savedChatPath(id: String) = "/chats/${segment(id)}/stream"

@Serializable
data class LiveBatch(
    val events: List<RunEvent>,
    val state: LiveState? = null,
    val reset: Boolean,
    val more: Boolean,
    val history: String? = null,
    val oldest: Long? = null,
    val hasOlder: Boolean = false,
    /** Timeline cards: the window's snapshot, or the cards these events changed. */
    val entries: List<TimelineChange> = emptyList(),
    val largeEntries: List<build.cairn.contracts.LargeTimelineReference> = emptyList(),
    /** On a compact stream: the list changes since the previous batch. */
    val chatChanges: ChatChanges? = null,
)

typealias HistoryPage = build.cairn.contracts.HistoryPage

/** A timeline card the server interpreted; the UI's own feed rows are `ui.TimelineEntry`. */
typealias TimelineCard = build.cairn.contracts.TimelineEntry

typealias TimelineChange = build.cairn.contracts.TimelineChange

typealias TimelineNotice = build.cairn.contracts.TimelineNotice

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
