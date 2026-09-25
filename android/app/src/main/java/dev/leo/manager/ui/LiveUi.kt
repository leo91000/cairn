package dev.leo.manager.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import dev.leo.manager.data.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.transformWhile
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.*

@Composable
fun rememberLive(
    vm: LeoViewModel,
    workspace: Workspace,
    path: String,
    streaming: Boolean = true,
): LiveSnapshot {
    val owner = LocalLifecycleOwner.current
    val enabled = workspace.session.authenticated && !workspace.signingOut
    val api = if (enabled) vm.api else null
    var value by remember(path, api) { mutableStateOf(LiveSnapshot()) }
    val session = remember(path, api) { LiveSession() }
    val scope = rememberCoroutineScope()
    var older by remember(path, api) { mutableStateOf(emptyList<RunEvent>()) }
    var olderHistory by remember(path, api) { mutableStateOf<String?>(null) }
    var boundary by remember(path, api) { mutableStateOf<Long?>(null) }
    var moreOlder by remember(path, api) { mutableStateOf(false) }
    var loadingOlder by remember(path, api) { mutableStateOf(false) }
    var olderError by remember(path, api) { mutableStateOf<String?>(null) }
    fun display(snapshot: LiveSnapshot): LiveSnapshot {
        if (snapshot.history != olderHistory) {
            older = emptyList()
            boundary = null
            olderHistory = snapshot.history
            olderError = null
        }
        return if (boundary == null) snapshot
        else
            snapshot.copy(
                events = mergeHistory(older, snapshot.events),
                oldest = boundary!!,
                hasOlder = moreOlder,
            )
    }
    LaunchedEffect(path, api, owner, streaming) {
        if (enabled && api != null) {
            var cacheGeneration = vm.historyCache.generation
            val key = vm.historyCache.key(workspace.origin, api.csrf, path)
            // A resumed session is already at least as recent as its cache.
            if (session.path != path)
                vm.historyCache.read(key)?.let {
                    session.restore(it, path, api.streamGeneration.get())
                    value = session.snapshot
                }
            try {
                owner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
                    try {
                        // A preview settles on its first complete snapshot, then releases the
                        // stream:
                        // a restored session emits it before connecting, so a cached preview never
                        // connects.
                        api.live(path, session)
                            .transformWhile {
                                emit(it)
                                streaming || (it.catchingUp && it.httpStatus == null)
                            }
                            .collect {
                                vm.acceptCacheRevision(it.state?.cacheRevision)
                                cacheGeneration = vm.historyCache.generation
                                if (!it.catchingUp || it.httpStatus != null) value = display(it)
                                if (it.httpStatus in listOf(401, 403, 404, 409)) {
                                    vm.historyCache.remove(key)
                                    value =
                                        LiveSnapshot(
                                            httpStatus = it.httpStatus,
                                            status = it.status,
                                            error = it.error,
                                        )
                                } else if (
                                    !it.catchingUp && it.state != null && it.history != null
                                ) {
                                    vm.historyCache.save(
                                        key,
                                        CachedHistory(
                                            it.cursor,
                                            it.history,
                                            it.state,
                                            value.events,
                                            oldest = value.oldest,
                                            hasOlder = value.hasOlder,
                                        ),
                                        expectedGeneration = cacheGeneration,
                                    )
                                }
                                if (it.httpStatus == 401)
                                    vm.report(ApiException(401, "Session expirée"))
                            }
                    } finally {
                        withContext(NonCancellable) { vm.historyCache.flush(key) }
                    }
                }
            } finally {
                withContext(NonCancellable) { vm.historyCache.flush(key) }
            }
        }
    }
    return value.copy(
        loadingOlder = loadingOlder,
        olderError = olderError,
        loadOlder = {
            val history = value.history
            if (enabled && api != null && history != null && value.hasOlder && !loadingOlder) {
                loadingOlder = true
                olderError = null
                val before = value.oldest
                val cacheGeneration = vm.historyCache.generation
                scope.launch {
                    var pageApplied = false
                    try {
                        val page =
                            api.get<HistoryPage>(
                                path.removeSuffix("/stream") +
                                    "/history?before=$before&history=${segment(history)}"
                            )
                        if (value.history == history && page.history == history) {
                            require(page.oldest < before || !page.hasOlder) {
                                "Page d’historique invalide."
                            }
                            older = mergeHistory(page.events, older)
                            olderHistory = history
                            boundary = page.oldest
                            moreOlder = page.hasOlder
                            value = display(value)
                            // Release paging in the same UI turn as insertion. Disk persistence
                            // must not leave the reader on the new rows while restoration waits.
                            pageApplied = true
                            loadingOlder = false
                            value.state?.let { detail ->
                                vm.historyCache.save(
                                    vm.historyCache.key(workspace.origin, api.csrf, path),
                                    CachedHistory(
                                        value.cursor,
                                        history,
                                        detail,
                                        value.events,
                                        oldest = value.oldest,
                                        hasOlder = value.hasOlder,
                                    ),
                                    expectedGeneration = cacheGeneration,
                                )
                            }
                        }
                    } catch (e: CancellationException) {
                        throw e
                    } catch (e: Exception) {
                        // A cache failure cannot undo an already applied server page.
                        if (!pageApplied) {
                            olderError = e.message ?: "Historique indisponible. Réessayez."
                            if (e is ApiException && e.status == 401) vm.report(e)
                        }
                    } finally {
                        // A later request may have started while this page was being cached.
                        if (!pageApplied) loadingOlder = false
                    }
                }
            }
        },
    )
}

/**
 * Inverted infinite scroll: prepend older pages ahead of the reader, several screens before the
 * start, and let Compose keep the first visible row in place by its key. A scrolled history never
 * gets a requested position, so a drag or fling in progress is never interrupted.
 */
@Composable
internal fun rememberHistoryPaging(
    live: LiveSnapshot,
    list: LazyListState,
    ready: Boolean,
): () -> Unit {
    val current by rememberUpdatedState(live)
    val load = { if (current.hasOlder && !current.loadingOlder) current.loadOlder() }
    val page = remember(list) { longArrayOf(live.oldest) }
    SideEffect {
        if (live.oldest != page[0]) {
            page[0] = live.oldest
            // A history shorter than the screen has no scrolled row to anchor on: keep its end in
            // place instead. Nothing can be scrolling, and the index is clamped to the last row.
            val info = list.layoutInfo
            if (info.totalItemsCount > 0 && !list.canScrollBackward && !list.canScrollForward)
                list.requestScrollToItem(Int.MAX_VALUE)
        }
    }
    LaunchedEffect(list, ready) {
        if (ready)
            snapshotFlow {
                val snapshot = current
                val info = list.layoutInfo
                // Pages can fold into only a few rows; keep loading until enough is buffered.
                val near =
                    info.visibleItemsInfo.isNotEmpty() &&
                        list.distanceToStart() < HISTORY_PREFETCH_SCREENS * info.viewportSize.height
                // The boundary changes with each page, even when a fast response is applied
                // before a loading frame was ever observed.
                (near &&
                    snapshot.hasOlder &&
                    !snapshot.loadingOlder &&
                    snapshot.olderError == null) to snapshot.oldest
            }
                .collect { (load) -> if (load) current.loadOlder() }
    }
    return load
}

internal const val HISTORY_PREFETCH_SCREENS = 3

/**
 * Estimated pixels above the viewport; unmeasured rows are assumed to be as tall as visible ones.
 */
internal fun LazyListState.distanceToStart(): Int {
    val info = layoutInfo
    val visible = info.visibleItemsInfo
    val first = visible.firstOrNull() ?: return 0
    val average = visible.sumOf { it.size + info.mainAxisItemSpacing } / visible.size
    return first.index * average + (info.viewportStartOffset - first.offset).coerceAtLeast(0)
}

/**
 * Connection and paging states are shown over the list rather than above it or as a row: a line
 * above would resize the list on every reconnect, and a row at the start would become Compose's
 * scroll anchor and push the reader's text down when a page arrives.
 */
@Composable
internal fun BoxScope.HistoryStatus(
    live: LiveSnapshot,
    list: LazyListState,
    retry: () -> Unit,
    connection: String? = null,
) {
    val atStart by remember(list) { derivedStateOf { !list.canScrollBackward } }
    val error = live.olderError
    Column(
        Modifier.align(Alignment.TopCenter).padding(top = 8.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        // Keep the last text while fading out, so the pill does not empty before it disappears.
        var shown by remember { mutableStateOf(connection.orEmpty()) }
        if (connection != null) shown = connection
        androidx.compose.animation.AnimatedVisibility(
            connection != null,
            enter = androidx.compose.animation.fadeIn(),
            exit = androidx.compose.animation.fadeOut(),
        ) {
            StatusPill {
                Text(
                    shown,
                    Modifier.widthIn(max = 280.dp),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2,
                )
            }
        }
        androidx.compose.animation.AnimatedVisibility(
            live.loadingOlder && atStart || error != null,
            enter = androidx.compose.animation.fadeIn(),
            exit = androidx.compose.animation.fadeOut(),
        ) {
            StatusPill {
                if (error == null) {
                    CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                    Text(
                        "Chargement des messages précédents…",
                        style = MaterialTheme.typography.labelMedium,
                    )
                } else {
                    Text(
                        error,
                        Modifier.widthIn(max = 220.dp),
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.error,
                        maxLines = 2,
                    )
                    TextButton(onClick = retry) { Text("Réessayer") }
                }
            }
        }
    }
}

@Composable
private fun StatusPill(content: @Composable RowScope.() -> Unit) {
    Surface(
        shape = RoundedCornerShape(50),
        color = MaterialTheme.colorScheme.surfaceContainerHigh,
        shadowElevation = 2.dp,
    ) {
        Row(
            Modifier.heightIn(min = 32.dp).padding(horizontal = 14.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            content = content,
        )
    }
}

/**
 * A page that ends inside the oldest activity group extends it, and its first event (the natural
 * key) changes. Reuse the key the group already had, so the list keeps anchoring on that row.
 */
internal class TimelineKeys {
    private var history: String? = null
    private var owners = mapOf<String, String>()

    fun stabilize(history: String?, entries: List<TimelineEntry>): List<TimelineEntry> {
        if (history != this.history) {
            this.history = history
            owners = emptyMap()
        }
        val used =
            entries.filter { !it.key.startsWith("activity:") }.mapTo(mutableSetOf()) { it.key }
        val keys = arrayOfNulls<String>(entries.size)
        // Event ids are unique; item ids only help when a folded update replaced the known event.
        for (identity in
            listOf<(RunEvent) -> String?>({ "event:${it.id}" }, { it.itemIdentity() })) {
            entries.forEachIndexed { index, entry ->
                if (keys[index] == null && entry.key.startsWith("activity:"))
                    entry.events
                        .firstNotNullOfOrNull { event ->
                            identity(event)?.let(owners::get)?.takeIf { it !in used }
                        }
                        ?.let {
                            keys[index] = it
                            used.add(it)
                        }
            }
        }
        val result = entries.mapIndexed { index, entry ->
            val key =
                keys[index]
                    ?: entry.key.takeIf { it !in used || !it.startsWith("activity:") }
                    ?: "${entry.key}:$index"
            used.add(key)
            if (key == entry.key) entry else entry.copy(key = key)
        }
        owners = buildMap {
            for (entry in result) if (entry.key.startsWith("activity:"))
                for (event in entry.events) {
                    put("event:${event.id}", entry.key)
                    event.itemIdentity()?.let { put(it, entry.key) }
                }
        }
        return result
    }

    private fun RunEvent.itemIdentity(): String? =
        item().string("id").takeIf { it.isNotBlank() }?.let { "item:${item().string("type")}:$it" }
}

/** Restore once the feed exists; background resumes retain the existing list state. */
@Composable
internal fun rememberHistoryPosition(
    vm: LeoViewModel,
    workspace: Workspace,
    path: String,
    live: LiveSnapshot,
    list: LazyListState,
    visible: Boolean,
    follow: Boolean,
    rendering: MarkdownRendering? = null,
    setFollow: (Boolean) -> Unit,
): Boolean {
    var ready by remember(path) { mutableStateOf(false) }
    val currentFollow by rememberUpdatedState(follow)
    val firstEvent by rememberUpdatedState(live.events.firstOrNull()?.id)
    val api = vm.api
    val key = remember(path, api) { vm.historyCache.key(workspace.origin, api.csrf, path) }
    LaunchedEffect(path, live.catchingUp, visible) {
        if (!ready && !live.catchingUp && visible) {
            val saved = live.position
            if (saved != null) {
                setFollow(saved.follow)
                if (!saved.follow && live.events.isNotEmpty()) {
                    restoreHistoryPosition(list, saved, rendering)
                }
            }
            ready = true
        }
    }
    LaunchedEffect(key, ready, visible) {
        if (ready && visible) {
            try {
                snapshotFlow {
                    ReadingPosition(
                        list.firstVisibleItemIndex,
                        list.firstVisibleItemScrollOffset,
                        currentFollow,
                        firstEvent,
                    )
                }
                    .collectLatest { vm.historyCache.position(key, it) }
            } finally {
                withContext(NonCancellable) {
                    vm.historyCache.position(
                        key,
                        ReadingPosition(
                            list.firstVisibleItemIndex,
                            list.firstVisibleItemScrollOffset,
                            currentFollow,
                            firstEvent,
                        ),
                    )
                    vm.historyCache.flush(key)
                }
            }
        }
    }
    return ready
}

internal fun RunEvent.item(): JsonObject? = activityData()?.get("item") as? JsonObject

internal fun JsonObject?.string(key: String) =
    this?.get(key)?.let { it as? JsonPrimitive }?.contentOrNull.orEmpty()

internal fun RunEvent.isMessage(): Boolean =
    type == "chat.user" ||
        item().string("type") == "agent_message" ||
        (activityData() == null && type == "item.completed" && item() == null)

internal data class TimelineEntry(
    val key: String,
    val events: List<RunEvent>,
    val message: Boolean,
    val files: List<Deliverable> = emptyList(),
    val notice: Boolean = false,
)

/** Fold updates in place within a turn; keep messages and errors in chronological order. */
internal fun timelineEntries(events: List<RunEvent>, chat: Boolean = false): List<TimelineEntry> {
    val folded = mutableListOf<RunEvent>()
    val positions = mutableMapOf<String, Int>()
    var legacyTool: Int? = null
    for (event in events) {
        if (event.type == "artifact") continue
        if (event.type == "turn.started") {
            positions.clear()
            legacyTool = null
        }
        val legacy =
            (event.type == "item.started" || event.type == "item.completed") &&
                event.activityData() == null
        if (legacy && event.type == "item.completed" && legacyTool != null) {
            val index = legacyTool
            val original = folded[index]
            folded[index] =
                event.copy(
                    id = original.id,
                    createdAt = original.createdAt,
                    payload =
                        mapOf(
                            "item" to
                                buildJsonObject {
                                    put("type", "saved_output")
                                    put("text", event.text)
                                }
                        ),
                )
            legacyTool = null
            continue
        }
        if (legacy && event.type == "item.started") legacyTool = folded.size
        val item = event.item()
        val id = item.string("id")
        if (!event.isMessage() && id.isNotBlank()) {
            val key = "${item.string("type")}:$id"
            val index = positions[key]
            if (index == null) {
                positions[key] = folded.size
                folded.add(event)
            } else folded[index] = event.copy(id = folded[index].id)
        } else folded.add(event)
    }
    val result = mutableListOf<TimelineEntry>()
    var activity = mutableListOf<RunEvent>()
    fun flushActivity() {
        if (activity.isNotEmpty()) {
            result.add(TimelineEntry("activity:${activity.first().id}", activity.toList(), false))
            activity = mutableListOf()
        }
    }
    val recovered =
        if (chat)
            recoveredChatConnections(events, folded.filter { it.isMessage() }.map { it.id }.toSet())
        else emptySet()
    for (event in folded) {
        if (event.isMessage()) {
            flushActivity()
            result.add(TimelineEntry("message:${event.displayId ?: event.id}", listOf(event), true))
        } else {
            val presentation = if (chat) presentActivity(event) else null
            if (presentation?.kind == ActivityKind.NOTICE) {
                val interrupted =
                    event.type == "status" &&
                        event.text.lowercase() in listOf("cancelled", "interrupted")
                if (
                    event.id !in recovered &&
                        (presentation.failed || interrupted || presentation.connectionInterrupted())
                ) {
                    flushActivity()
                    result.add(
                        TimelineEntry("notice:${event.id}", listOf(event), false, notice = true)
                    )
                }
            } else activity.add(event)
        }
    }
    flushActivity()
    return result
}

/**
 * Match the web feed: attach each publication beside its response, keeping versions per message.
 */
internal fun deliveryTimeline(
    entries: List<TimelineEntry>,
    artifacts: List<Deliverable>,
): List<TimelineEntry> {
    val groups = linkedMapOf<Int, MutableList<Deliverable>>()
    val latest =
        artifacts
            .groupBy { it.messageId.orEmpty() to it.key }
            .values
            .map { it.maxBy { file -> file.version } }
    for (file in latest) {
        var position = entries.lastIndex
        for ((index, entry) in entries.withIndex()) {
            if (!entry.message) continue
            val event = entry.events.single()
            val time =
                (event.payload?.get("createdAt") as? JsonPrimitive)?.longOrNull?.takeIf {
                    it > 0 && it <= event.createdAt
                } ?: event.createdAt
            if (time < file.createdAt) continue
            position = if (event.type == "chat.user") index - 1 else index
            break
        }
        groups.getOrPut(position) { mutableListOf() }.add(file)
    }
    return buildList {
        fun append(index: Int) {
            groups[index]?.let {
                add(
                    TimelineEntry(
                        "files:${entries.getOrNull(index)?.key ?: "start"}",
                        emptyList(),
                        false,
                        it,
                    )
                )
            }
        }
        append(-1)
        entries.forEachIndexed { index, entry ->
            add(entry)
            append(index)
        }
    }
}

@Composable
internal fun TimelineRow(
    vm: LeoViewModel,
    entry: TimelineEntry,
    agent: String,
    rendering: MarkdownRendering? = null,
    /** While the agent works, its step in progress is shown by the working indicator instead. */
    hideRunning: Boolean = false,
) {
    if (entry.files.isNotEmpty()) ArtifactStrip(vm, entry.files)
    else if (entry.message)
        CompositionLocalProvider(LocalMarkdownRendering provides rendering) {
            EventRow(vm, entry.events.single(), agent)
        }
    else if (entry.notice) ChatNotice(presentActivity(entry.events.single()))
    else {
        val presentations = remember(entry.events) { entry.events.map(::presentActivity) }
        AgentActions(entry.key, presentations, hideRunning)
    }
}

@Composable
fun EventRow(vm: LeoViewModel, event: RunEvent, agent: String = "Leo") {
    val item = event.item()
    val user = event.type == "chat.user"
    if (event.isMessage()) {
        val content =
            if (user) (event.payload?.get("text") as? JsonPrimitive)?.contentOrNull ?: event.text
            else item.string("text").ifEmpty { event.text }
        BoxWithConstraints(
            Modifier.fillMaxWidth(),
            contentAlignment = if (user) Alignment.CenterEnd else Alignment.CenterStart,
        ) {
            Surface(
                color =
                    if (user) MaterialTheme.colorScheme.surfaceContainerHigh
                    else MaterialTheme.colorScheme.background,
                shape =
                    RoundedCornerShape(
                        topStart = 16.dp,
                        topEnd = 16.dp,
                        bottomStart = 16.dp,
                        bottomEnd = 4.dp,
                    ),
                modifier =
                    if (user) Modifier.widthIn(max = maxWidth * 0.9f) else Modifier.fillMaxWidth(),
            ) {
                Column(
                    Modifier.padding(
                        horizontal = if (user) 16.dp else 0.dp,
                        vertical = if (user) 12.dp else 8.dp,
                    ),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    if (!user)
                        Row(
                            Modifier.fillMaxWidth(),
                            horizontalArrangement = Arrangement.SpaceBetween,
                        ) {
                            Text(
                                agent,
                                Modifier.weight(1f),
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            Text(
                                java.text
                                    .SimpleDateFormat(
                                        "HH:mm",
                                        androidx.compose.ui.platform.LocalConfiguration.current
                                            .locales[0],
                                    )
                                    .format(java.util.Date(event.createdAt)),
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    if (user)
                        androidx.compose.foundation.text.selection.SelectionContainer {
                            Text(
                                highlightSkills(
                                    content,
                                    LocalSkillNames.current,
                                    skillMentionStyle(),
                                ),
                                style = MaterialTheme.typography.bodyLarge,
                            )
                        }
                    else Markdown(content)
                    if (user)
                        Text(
                            java.text
                                .SimpleDateFormat(
                                    "HH:mm",
                                    androidx.compose.ui.platform.LocalConfiguration.current.locales[
                                            0],
                                )
                                .format(java.util.Date(event.createdAt)),
                            Modifier.align(Alignment.End),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    val attachments = runCatching {
                        event.payload?.get("attachments")?.let {
                            wireJson.decodeFromJsonElement<List<ChatAttachment>>(it)
                        }
                    }
                        .getOrNull()
                        .orEmpty()
                    if (attachments.isNotEmpty()) AttachmentList(vm, attachments)
                }
            }
        }
    } else ActivityCard(event)
}

internal suspend fun restoreHistoryPosition(
    list: LazyListState,
    saved: ReadingPosition,
    rendering: MarkdownRendering?,
) {
    snapshotFlow { list.layoutInfo.totalItemsCount }.first { it > 0 }
    val index = saved.index.coerceIn(0, list.layoutInfo.totalItemsCount - 1)
    list.scrollToItem(index, saved.offset.coerceAtLeast(0))
    rendering?.awaitLayout()
    list.scrollToItem(index, saved.offset.coerceAtLeast(0))
}
