package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.hideFromAccessibility
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import build.cairn.app.data.*
import build.cairn.app.diagnostics.PerfOperation
import build.cairn.app.diagnostics.Performance
import build.cairn.contracts.ReadRunHistoryInput
import build.cairn.contracts.TimelineContent
import build.cairn.contracts.readRunHistory
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.transformWhile
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.*

/** How long a revalidation of a saved conversation may take before the reader is told. */
private const val SLOW_UPDATE_MILLIS = 1000L

@Composable
fun rememberLive(
    vm: CairnViewModel,
    workspace: Workspace,
    path: String,
    streaming: Boolean = true,
    /**
     * Whether a saved conversation still matches what the server holds. An outdated one only
     * resumes the stream from its cursor: the reader waits for the update instead of seeing older
     * output that is about to change, as on the web. Previews always show their copy.
     */
    current: (CachedHistory) -> Boolean = { true },
): LiveSnapshot {
    val owner = LocalLifecycleOwner.current
    val isCurrent by rememberUpdatedState(current)
    val enabled = workspace.session.authenticated && !workspace.signingOut
    val api = if (enabled) vm.api else null
    var value by remember(path, api) { mutableStateOf(LiveSnapshot()) }
    val session = remember(path, api) { LiveSession() }
    // Retained pager pages become previews and then selected again without being restored.
    // Recheck their latest snapshot at selection, before a frame of old content can show.
    var waiting by
        remember(path, api, streaming) {
            val previous = session.snapshot
            val detail = previous.state
            val history = previous.history
            val saved =
                if (detail != null && history != null)
                    CachedHistory(
                        previous.cursor,
                        history,
                        detail,
                        previous.events,
                        previous.entries,
                    )
                else null
            mutableStateOf(streaming && saved != null && !current(saved))
        }
    val parentScope = rememberCoroutineScope()
    val scope =
        remember(path, api) {
            CoroutineScope(parentScope.coroutineContext + Job(parentScope.coroutineContext[Job]))
        }
    DisposableEffect(scope) { onDispose { scope.cancel() } }
    val cacheWriter =
        remember(path, api) {
            api?.let {
                HistoryCacheWriter(
                    vm.historyCache,
                    vm.historyCache.key(it.cacheScope, it.csrf, path),
                    scope,
                )
            }
        }
    DisposableEffect(cacheWriter) { onDispose { cacheWriter?.close() } }
    val pages = remember(path, api) { OlderPages() }
    var loadingOlder by remember(path, api) { mutableStateOf(false) }
    var olderError by remember(path, api) { mutableStateOf<String?>(null) }

    LaunchedEffect(path, api, owner, streaming) {
        if (enabled && api != null) {
            var cacheGeneration = vm.historyCache.generation
            val key = vm.historyCache.key(api.cacheScope, api.csrf, path)
            // A resumed session is already at least as recent as its cache.
            if (session.path != path)
                vm.historyCache.read(key)?.let {
                    session.restore(it, path, api.streamGeneration.get())
                    waiting = streaming && !isCurrent(it)
                    if (!waiting) value = session.snapshot
                }
            val replay = session.snapshot
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
                                // The stream's own state replaces an outdated copy; without a
                                // connection, the copy is still better than nothing.
                                if (waiting && it === replay) return@collect
                                vm.acceptCacheRevision(it.state?.cacheRevision)
                                cacheGeneration = vm.historyCache.generation
                                if (!it.catchingUp || it.httpStatus != null) {
                                    if (it.history != value.history || it.epoch != value.epoch)
                                        olderError = null
                                    value = pages.display(it)
                                    waiting = false
                                }
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
                                    cacheWriter?.offer(
                                        CachedHistory(
                                            it.cursor,
                                            it.history,
                                            it.state,
                                            value.events,
                                            value.entries,
                                            oldest = value.oldest,
                                            hasOlder = value.hasOlder,
                                        ),
                                        expectedGeneration = cacheGeneration,
                                    )
                                }
                                if (it.httpStatus in listOf(401, 403, 404))
                                    vm.report(
                                        ApiException(
                                            checkNotNull(it.httpStatus),
                                            it.error ?: "Accès indisponible",
                                        )
                                    )
                            }
                    } finally {
                        withContext(NonCancellable) { cacheWriter?.flush() }
                    }
                }
            } finally {
                withContext(NonCancellable) { cacheWriter?.flush() }
            }
        }
    }
    val visible = if (waiting) LiveSnapshot() else value
    return visible.copy(
        loadingOlder = loadingOlder,
        olderError = olderError,
        loadOlder = {
            val requested = value
            val history = requested.history
            if (enabled && api != null && history != null && value.hasOlder && !loadingOlder) {
                loadingOlder = true
                olderError = null
                val before = requested.oldest
                val cacheGeneration = vm.historyCache.generation
                scope.launch {
                    var pageApplied = false
                    try {
                        val runId =
                            Regex("^/runs/([^/]+)/stream$").matchEntire(path)?.groupValues?.get(1)
                        val page =
                            if (runId != null) {
                                api.readRunHistory(
                                    ReadRunHistoryInput(
                                        runId = runId,
                                        before = before,
                                        history = history,
                                    )
                                )
                            } else
                                api.get<HistoryPage>(
                                    path.removeSuffix("/stream") +
                                        "/history?before=$before&history=${segment(history)}"
                                )

                        val applied = pages.apply(page, value, requested)
                        if (applied != null) {
                            value = applied
                            // Release paging in the same UI turn as insertion. Disk persistence
                            // must not leave the reader on the new rows while restoration waits.
                            pageApplied = true
                            loadingOlder = false
                            value.state?.let { detail ->
                                cacheWriter?.offer(
                                    CachedHistory(
                                        value.cursor,
                                        history,
                                        detail,
                                        value.events,
                                        value.entries,
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
                            if (e is ApiException && e.status in setOf(401, 403, 404)) vm.report(e)
                        }
                    } finally {
                        // A later request may have started while this page was being cached.
                        if (!pageApplied) loadingOlder = false
                    }
                }
            }
        },
        followLatest = { session.followsLatest = it },
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
        Modifier.align(Alignment.TopCenter).padding(start = 12.dp, top = 8.dp, end = 12.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        // A revalidation that takes a moment says so; a quick one stays silent, as on the web.
        var notice by remember { mutableStateOf(connection.takeIf { it != UPDATING_STATUS }) }
        LaunchedEffect(connection) {
            if (connection == UPDATING_STATUS) delay(SLOW_UPDATE_MILLIS)
            notice = connection
        }
        // Keep the last text while fading out, so the pill does not empty before it disappears.
        var shown by remember { mutableStateOf(notice.orEmpty()) }
        notice?.let { shown = it }
        androidx.compose.animation.AnimatedVisibility(
            notice != null,
            enter = androidx.compose.animation.fadeIn(),
            exit = androidx.compose.animation.fadeOut(),
        ) {
            StatusPill {
                Text(
                    shown,
                    Modifier.widthIn(max = 280.dp).padding(vertical = 6.dp),
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
            // Like the web's role="status", accessibility services hear paging progress and errors.
            StatusPill(Modifier.semantics { liveRegion = LiveRegionMode.Polite }) {
                if (error == null) {
                    CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                    Text(
                        "Chargement des messages précédents…",
                        Modifier.padding(vertical = 6.dp),
                        style = MaterialTheme.typography.labelMedium,
                    )
                } else {
                    // The retry action keeps its whole label; the error wraps beside it.
                    Text(
                        error,
                        Modifier.weight(1f, fill = false)
                            .widthIn(max = 220.dp)
                            .padding(vertical = 6.dp),
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.error,
                        overflow = TextOverflow.Ellipsis,
                        maxLines = 3,
                    )
                    TextButton(
                        onClick = retry,
                        contentPadding = PaddingValues(horizontal = 8.dp),
                    ) {
                        Text("Réessayer", softWrap = false)
                    }
                }
            }
        }
    }
}

@Composable
private fun StatusPill(
    modifier: Modifier = Modifier,
    content: @Composable RowScope.() -> Unit,
) {
    // A capsule on one line; with large text, a rounded rectangle that keeps wrapped lines inside.
    Surface(
        modifier,
        shape = RoundedCornerShape(24.dp),
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
 * key) changes. Teams can also gain an earlier member. Reuse the key the group already had, so the
 * list keeps anchoring on that row.
 */
internal class TimelineKeys {
    private var history: String? = null
    private var owners = mapOf<Long, String>()
    private var fullPositions = mapOf<Long, Int>()
    private var teamKeys = SubagentTeamKeys()

    fun stabilize(history: String?, input: List<TimelineEntry>): List<TimelineEntry> {
        if (history != this.history) {
            this.history = history
            owners = emptyMap()
            fullPositions = emptyMap()
            teamKeys = SubagentTeamKeys()
        }
        val identities =
            teamKeys.stabilize(input.filter { it.agents.isNotEmpty() }.map { it.agents }).iterator()
        val entries =
            regroup(
                input.map { entry ->
                    if (entry.agents.isEmpty()) entry
                    else entry.copy(key = "team:${identities.next()}")
                }
            )
        val used =
            entries.filter { !it.key.startsWith("activity:") }.mapTo(mutableSetOf()) { it.key }
        val keys = arrayOfNulls<String>(entries.size)
        // Match any persisted event folded into the activity, including older starts.
        entries.forEachIndexed { index, entry ->
            if (entry.key.startsWith("activity:"))
                entry.events
                    .firstNotNullOfOrNull { event ->
                        entry.sources(event).firstNotNullOfOrNull {
                            owners[it]?.takeIf { key -> key !in used }
                        }
                    }
                    ?.let {
                        keys[index] = it
                        used.add(it)
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
                    for (id in entry.sources(event)) put(id, entry.key)
                }
        }
        return result
    }

    /** Keep full visible groups intact when older pages arrive or new tools append. */
    private fun regroup(entries: List<TimelineEntry>): List<TimelineEntry> {
        val result = mutableListOf<TimelineEntry>()
        val nextPositions = mutableMapOf<Long, Int>()
        val activity = mutableListOf<RunEvent>()
        val sources = mutableMapOf<Long, List<Long>>()
        val notices = mutableMapOf<Long, build.cairn.app.data.TimelineNotice>()
        fun identities(event: RunEvent) = sources[event.id] ?: listOf(event.id)
        fun previous(event: RunEvent) = identities(event).firstNotNullOfOrNull(fullPositions::get)
        fun flush() {
            if (activity.isEmpty()) return
            // Older starts may move the oldest tool around a notice; anchor from the end.
            val anchor = activity.indexOfLast { previous(it) != null }
            val offset = if (anchor < 0) 0 else checkNotNull(previous(activity[anchor])) - anchor
            var group = mutableListOf<RunEvent>()
            var previousWindow: Int? = null
            fun append() {
                if (group.isEmpty()) return
                result.add(
                    TimelineEntry(
                        "activity:${group.first().id}",
                        group.toList(),
                        false,
                        sourceIds = group.associate { it.id to identities(it) },
                        notices =
                            group.mapNotNull { e -> notices[e.id]?.let { e.id to it } }.toMap(),
                    )
                )
                if (group.size == MAX_ACTIVITY_GROUP_SIZE) {
                    for ((position, event) in group.withIndex()) {
                        for (id in identities(event)) nextPositions[id] = position
                    }
                }
                group = mutableListOf()
            }
            for ((index, event) in activity.withIndex()) {
                val window = Math.floorDiv(offset + index, MAX_ACTIVITY_GROUP_SIZE)
                if (previousWindow != null && previousWindow != window) append()
                group.add(event)
                previousWindow = window
            }
            append()
            activity.clear()
        }
        for (entry in entries) {
            if (entry.key.startsWith("activity:")) {
                activity.addAll(entry.events)
                sources.putAll(entry.sourceIds)
                notices.putAll(entry.notices)
            } else {
                flush()
                result.add(entry)
            }
        }
        flush()
        // Retain only anchors in the loaded timeline. Partial edge groups can still grow.
        fullPositions = nextPositions
        return result
    }

    private fun TimelineEntry.sources(event: RunEvent): List<Long> =
        sourceIds[event.id] ?: listOf(event.id)
}

/** Restore once the feed exists; background resumes retain the existing list state. */
@Composable
internal fun rememberHistoryPosition(
    vm: CairnViewModel,
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
    val followLatest by rememberUpdatedState(live.followLatest)
    val api = vm.api
    val key = remember(path, api) { vm.historyCache.key(api.cacheScope, api.csrf, path) }
    LaunchedEffect(path, live.catchingUp, visible) {
        if (!ready && !live.catchingUp && visible) {
            val saved = live.position
            if (saved != null) {
                setFollow(saved.follow)
                if (!saved.follow && live.events.isNotEmpty()) {
                    restoreHistoryPosition(list, saved, rendering)
                }
            }
            if (saved?.follow ?: currentFollow) {
                // Measure the latest rows while hidden, including their async Markdown, before
                // exposing a position or allowing the paging hook to load older messages.
                list.scrollToItem(Int.MAX_VALUE)
                rendering?.awaitLayout()
                val last = (list.layoutInfo.totalItemsCount - 1).coerceAtLeast(0)
                list.scrollToItem(last, Int.MAX_VALUE)
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
                    .collectLatest {
                        // Published only once restored, never masking a saved reader.
                        followLatest(it.follow)
                        vm.historyCache.position(key, it)
                    }
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

/** Keep measuring during restoration without painting intermediate reading positions. */
internal fun Modifier.historyVisibility(ready: Boolean): Modifier = graphicsLayer {
    alpha = if (ready) 1f else 0f
}
    .semantics { if (!ready) hideFromAccessibility() }

internal fun RunEvent.item(): JsonObject? = activityData()?.get("item") as? JsonObject

internal fun JsonObject?.string(key: String) =
    this?.get(key)?.let { it as? JsonPrimitive }?.contentOrNull.orEmpty()

internal data class TimelineEntry(
    val key: String,
    val events: List<RunEvent>,
    val message: Boolean,
    val files: List<Deliverable> = emptyList(),
    val notice: Boolean = false,
    /** The server's card for a message or a conversation notice. */
    val card: TimelineCard? = null,
    /** Run-log presentation of the lifecycle events in an activity group, by event. */
    val notices: Map<Long, build.cairn.app.data.TimelineNotice> = emptyMap(),
    /** Persisted events folded into each activity; item IDs may repeat in later turns. */
    val sourceIds: Map<Long, List<Long>> = emptyMap(),
    /** Subagents started one after another work as a team, shown apart from the agent's steps. */
    val agents: List<Subagent> = emptyList(),
    /** A scheduled wait, shown as one card from its scheduling to its resumption. */
    val wait: ActivityWait? = null,
    /** The agent's plan, kept up to date by its later updates. */
    val plan: AgentPlan? = null,
    /** Where an update changed the plan. */
    val planUpdate: PlanUpdate? = null,
    /** The coding agent compacted its context; the history above stays as it was. */
    val compaction: ContextCompaction? = null,
)

/**
 * Fold tool updates in place within a turn. Messages, the run cycle, errors and retries arrive as
 * cards the server interpreted; they take their place among the activity, and the events they
 * present only mark the boundaries of the families still read here.
 */
internal fun timelineEntries(
    events: List<RunEvent>,
    chat: Boolean = false,
    pendingWakeup: ChatWakeup? = null,
    cards: List<TimelineCard> = emptyList(),
): List<TimelineEntry> =
    Performance.measure(PerfOperation.TIMELINE) {
        val folded = mutableListOf<RunEvent>()
        val sources = mutableListOf<MutableList<Long>>()
        val positions = mutableMapOf<String, Int>()
        val subagents = subagentTimeline(events)
        for (event in events) {
            // Context usage is a measurement the conversation header shows, not an activity.
            if (event.type == "artifact" || event.type == "context.usage") continue
            if (event.id in subagents.captured) {
                if (event.id in subagents.anchors) {
                    folded.add(event)
                    sources.add(mutableListOf(event.id))
                }
                continue
            }
            if (event.type == "turn.started") {
                positions.clear()
            }
            val item = event.item()
            val id = item.string("id")
            // Each plan update keeps its place: it shows what changed at that moment.
            val planUpdate = item.string("type") == "todo_list"
            if (event.timeline != true && id.isNotBlank() && !planUpdate) {
                val key = "${item.string("type")}:$id"
                val index = positions[key]
                if (index == null) {
                    positions[key] = folded.size
                    folded.add(event)
                    sources.add(mutableListOf(event.id))
                } else {
                    folded[index] = event.copy(id = folded[index].id)
                    sources[index].add(event.id)
                }
            } else {
                folded.add(event)
                sources.add(mutableListOf(event.id))
            }
        }
        val sourceIds =
            folded.mapIndexed { index, event -> event.id to sources[index].toList() }.toMap()
        // The run log presents a lifecycle event through its card; a conversation shows the
        // cards the server marked for it.
        val runLog =
            if (chat) emptyMap()
            else
                cards
                    .mapNotNull { card ->
                        (card.content as? TimelineContent.C)?.let { card.source to it.notice }
                    }
                    .toMap()
        val result = mutableListOf<TimelineEntry>()
        var activity = mutableListOf<RunEvent>()
        var team: Int? = null
        fun flushActivity() {
            team = null
            if (activity.isNotEmpty()) {
                result.add(
                    TimelineEntry(
                        "activity:${activity.first().id}",
                        activity,
                        false,
                        sourceIds = activity.associate { it.id to sourceIds.getValue(it.id) },
                        notices =
                            activity.mapNotNull { e -> runLog[e.id]?.let { e.id to it } }.toMap(),
                    )
                )
                activity = mutableListOf()
            }
        }
        var nextCard = 0
        fun placeCards(before: Long) {
            while (nextCard < cards.size && cards[nextCard].position < before) {
                val card = cards[nextCard++]
                when (val content = card.content) {
                    is TimelineContent.A -> {
                        flushActivity()
                        result.add(TimelineEntry(card.id, emptyList(), true, card = card))
                    }
                    // A reply repeating a failure the conversation reports stays in the run log.
                    is TimelineContent.B ->
                        if (!chat || content.echo != true) {
                            flushActivity()
                            result.add(TimelineEntry(card.id, emptyList(), true, card = card))
                        }
                    is TimelineContent.C ->
                        if (chat && content.notice.shown()) {
                            flushActivity()
                            result.add(
                                TimelineEntry(
                                    card.id,
                                    emptyList(),
                                    false,
                                    notice = true,
                                    card = card,
                                )
                            )
                        } else if (!chat && folded.none { it.id == card.source }) {
                            // A card can change in this window while its source is on an older
                            // page.
                            flushActivity()
                            result.add(TimelineEntry(card.id, emptyList(), false, card = card))
                        }
                }
            }
        }
        // The plan its updates change, until the agent finishes it or writes another one.
        var planIndex: Int? = null
        // By run and item: a compaction's start and end update one divider.
        val compactions = mutableMapOf<String, Int>()
        // The wait still pending, by its wakeup ID, so that its result and resumption complete it.
        val waits = mutableMapOf<String, Int>()
        var openWait: Int? = null
        fun updateWait(index: Int, change: (ActivityWait) -> ActivityWait) {
            result[index] = result[index].copy(wait = change(result[index].wait!!))
        }
        // A scheduled wait only starts once its turn ends: its card follows what the agent did
        // after the call, and moves to where the turn ends, another wait or a message comes, or
        // the history does.
        var scheduled: Int? = null
        val moves = mutableMapOf<Int, Int>()
        fun placeScheduled() {
            val index = scheduled ?: return
            flushActivity()
            moves[index] = result.size
            scheduled = null
        }
        for ((eventIndex, event) in folded.withIndex()) {
            placeCards(event.id)
            // A compaction still running when its turn or run ends did not complete.
            compactionEnd(event.type, event.text)?.let { end ->
                for ((key, index) in compactions) {
                    val compaction = result[index].compaction ?: continue
                    if (
                        compaction.state == "running" &&
                            key.startsWith("compaction:${event.runId}:")
                    )
                        result[index] =
                            result[index].copy(compaction = compaction.copy(state = end))
                }
            }
            if (
                event.type in WAIT_BOUNDARIES ||
                    (event.type == "status" && event.text in TERMINAL_STATUSES)
            )
                placeScheduled()
            when (event.type) {
                "chat.sleep",
                "chat.wait",
                "run.wait" -> {
                    val wakeup = event.wakeupPayload() ?: continue
                    if (wakeup.id in waits) continue
                    openWait?.let { index -> updateWait(index) { it.copy(ended = "replaced") } }
                    openWait = result.size
                    scheduled = result.size
                    waits[wakeup.id] = result.size
                    result.add(
                        TimelineEntry(
                            "wait:${wakeup.id}",
                            listOf(event),
                            false,
                            wait = ActivityWait(wakeup),
                        )
                    )
                    continue
                }
                // The condition completed: right away (the agent never waited) or while it waited.
                "chat.wait_ready",
                "run.wait_ready" -> {
                    val observation = event.activityData()?.let(::presentWaitObservation)
                    // Right away, the agent never waited, and this call replaced any wait.
                    val immediate = observation?.get("immediate") == JsonPrimitive(true)
                    if (immediate) {
                        openWait?.let { index -> updateWait(index) { it.copy(ended = "replaced") } }
                        openWait = null
                    }
                    // The resumption or live snapshot identifies a wait scheduled outside history.
                    val resumed =
                        folded
                            .drop(eventIndex + 1)
                            .takeWhile {
                                it.type !in
                                    listOf("chat.user", "chat.wait", "chat.sleep", "run.wait")
                            }
                            .firstOrNull {
                                it.type in listOf("chat.wakeup", "run.wakeup") &&
                                    (it.payload?.get("observation") as? JsonObject)?.let(
                                        ::presentWaitObservation
                                    ) == observation
                            }
                            ?.wakeupPayload()
                    val open =
                        openWait?.let { result[it].wait?.wakeup }?.takeIf { it.condition != null }
                    val live = pendingWakeup?.takeIf {
                        it.condition != null && it.createdAt <= event.createdAt
                    }
                    val target = if (immediate) null else open ?: resumed ?: live
                    var pending = target?.id?.let(waits::get)
                    if (pending == null && target != null) {
                        flushActivity()
                        pending = result.size
                        waits[target.id] = pending
                        result.add(
                            TimelineEntry(
                                "wait:${target.id}",
                                listOf(event),
                                false,
                                wait = ActivityWait(target),
                            )
                        )
                    }
                    if (pending != null)
                        updateWait(pending) { it.copy(observation = observation, observed = true) }
                    else {
                        flushActivity()
                        result.add(
                            TimelineEntry(
                                "wait-ready:${event.id}",
                                listOf(event),
                                false,
                                wait =
                                    ActivityWait(
                                        null,
                                        observation,
                                        observed = true,
                                        // The result carries the reason.
                                        reason =
                                            if (immediate) observation.string("reason") else "",
                                    ),
                            )
                        )
                    }
                    continue
                }
                "chat.wakeup",
                "run.wakeup" -> {
                    val wakeup = event.wakeupPayload()
                    // Its scheduling may be in an older page of the history.
                    val index =
                        wakeup?.id?.let(waits::get)
                            ?: run {
                                flushActivity()
                                result.add(
                                    TimelineEntry(
                                        "wait:${wakeup?.id ?: event.id}",
                                        listOf(event),
                                        false,
                                        wait = ActivityWait(wakeup),
                                    )
                                )
                                wakeup?.id?.let { waits[it] = result.lastIndex }
                                result.lastIndex
                            }
                    updateWait(index) {
                        it.copy(
                            observation = wakeup?.observationJson ?: it.observation,
                            observed = true,
                            resumedAt = event.createdAt,
                        )
                    }
                    if (index == openWait) openWait = null
                    continue
                }
                // A new message cancels the pending wakeup.
                "chat.user" -> {
                    openWait?.let { index -> updateWait(index) { it.copy(ended = "cancelled") } }
                    openWait = null
                    // A new request gets its own plan, even when the agent carries the previous
                    // list over.
                    planIndex = null
                }
            }
            // A run that ends without succeeding never resumes the wait it scheduled. Only its
            // terminal status says so: a failed turn may be retried, and the retry keeps the
            // wakeup.
            val failedTurn =
                event.type == "status" && event.text in setOf("failed", "cancelled", "interrupted")
            openWait
                ?.takeIf { failedTurn && result[it].wait?.observed != true }
                ?.let { index ->
                    updateWait(index) { it.copy(ended = "cancelled") }
                    openWait = null
                }
            // The timeline presents this event: its card takes its place here. The run log keeps
            // a lifecycle event among the activity, presented by its card.
            if (event.timeline == true) {
                placeCards(event.id + 1)
                if (event.id in runLog) {
                    if (activity.size == MAX_ACTIVITY_GROUP_SIZE) flushActivity()
                    team = null
                    activity.add(event)
                }
                continue
            }
            val waitTool =
                event.item().let { it.string("type") == "mcp_tool_call" && isWaitTool(it) }
            val delegateTool =
                event.item().let { it.string("type") == "mcp_tool_call" && isDelegateTool(it) }
            if (delegateTool && !presentActivity(event).failed) continue
            // The wait shows as its own card; only a call that failed stays among the tools.
            if (waitTool && !presentActivity(event).failed) {
                continue
            }
            val compactionItem =
                event.item()?.takeIf {
                    it.string("type") == "context_compaction" && event.type.startsWith("item.")
                }
            if (compactionItem != null) {
                val key =
                    "compaction:${event.runId}:${compactionItem.string("id").ifBlank { event.id.toString() }}"
                val index = compactions[key]
                val compaction =
                    contextCompaction(
                        compactionItem,
                        event.type,
                        index?.let { result[it].compaction },
                    )
                if (index == null) {
                    flushActivity()
                    result.add(TimelineEntry(key, listOf(event), false, compaction = compaction))
                    compactions[key] = result.lastIndex
                } else result[index] = result[index].copy(compaction = compaction)
                continue
            }
            // The agent's own plan shows apart from its tools. A call that failed did not change
            // it.
            val planItem =
                event.item()?.takeIf {
                    it.string("type") == "todo_list" && it.string("parent_id").isBlank()
                }
            if (planItem != null) {
                val steps = planSteps(planItem)
                if (steps.isEmpty() || planItem.string("status") == "failed") continue
                val current = planIndex?.let { result[it].plan }
                val explanation = planItem.string("explanation")
                if (current != null && samePlan(current.steps, steps)) {
                    val updatedExplanation = explanation.ifBlank { current.explanation }
                    if (steps != current.steps || updatedExplanation != current.explanation) {
                        val index = checkNotNull(planIndex)
                        result[index] =
                            result[index].copy(
                                plan =
                                    current.copy(
                                        steps = steps,
                                        explanation = updatedExplanation,
                                        updatedAt = event.createdAt,
                                    )
                            )
                    }
                    continue
                }
                if (current == null || !continuesPlan(current.steps, steps)) {
                    flushActivity()
                    planIndex = result.size
                    result.add(
                        TimelineEntry(
                            "plan:${event.id}",
                            listOf(event),
                            false,
                            plan = AgentPlan(steps, explanation, event.createdAt, event.createdAt),
                        )
                    )
                    continue
                }
                val index = checkNotNull(planIndex)
                val change = planChange(current.steps, steps)
                result[index] =
                    result[index].copy(
                        plan =
                            current.copy(
                                steps = steps,
                                explanation = explanation.ifBlank { current.explanation },
                                updatedAt = event.createdAt,
                            )
                    )
                if (change != null) {
                    flushActivity()
                    result.add(
                        TimelineEntry(
                            "plan-update:${event.id}",
                            listOf(event),
                            false,
                            planUpdate = PlanUpdate(result[index].key, change, steps.progress()),
                        )
                    )
                }
                continue
            }
            val agent = subagents.anchors[event.id]
            if (agent != null) {
                val index = team
                if (index == null) {
                    flushActivity()
                    result.add(
                        TimelineEntry(
                            "team:${agent.id}",
                            listOf(event),
                            false,
                            agents = listOf(agent),
                        )
                    )
                    team = result.lastIndex
                } else {
                    val entry = result[index]
                    result[index] =
                        entry.copy(events = entry.events + event, agents = entry.agents + agent)
                }
                continue
            }
            // An engine marker Cairn does not know yet stays in the run log.
            if (chat && presentActivity(event).let { it.kind == ActivityKind.NOTICE && !it.failed })
                continue
            if (activity.size == MAX_ACTIVITY_GROUP_SIZE) {
                flushActivity()
            }
            team = null
            activity.add(event)
        }
        placeCards(Long.MAX_VALUE)
        flushActivity()
        placeScheduled()
        val knownWait = pendingWakeup?.id?.let(waits::get)
        if (knownWait != null && pendingWakeup.observation != null) {
            updateWait(knownWait) {
                if (it.observed) it
                else it.copy(observation = pendingWakeup.observationJson, observed = true)
            }
        }
        if (pendingWakeup != null && knownWait == null) {
            result.add(
                TimelineEntry(
                    "wait:${pendingWakeup.id}",
                    emptyList(),
                    false,
                    wait =
                        ActivityWait(
                            pendingWakeup,
                            pendingWakeup.observationJson,
                            observed = pendingWakeup.observation != null,
                        ),
                )
            )
        }
        val placed =
            if (moves.isEmpty()) result
            else {
                // Each moved card goes before the entry at its target, in one pass.
                val arriving = moves.entries.groupBy({ it.value }, { it.key })
                buildList(result.size) {
                    for (index in 0..result.size) {
                        arriving[index]?.sorted()?.forEach { add(result[it]) }
                        if (index < result.size && index !in moves) add(result[index])
                    }
                }
            }
        placed
    }

// Where a scheduled wait's card is placed: its turn ended, or a message or another wait event came.
private val WAIT_BOUNDARIES =
    setOf(
        "turn.started",
        "turn.completed",
        "turn.failed",
        "chat.user",
        "chat.sleep",
        "chat.wait",
        "chat.wait_ready",
        "chat.wakeup",
        "run.wait",
        "run.wait_ready",
        "run.wakeup",
    )
internal val TERMINAL_STATUSES = setOf("succeeded", "failed", "cancelled", "interrupted")

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
            val card = entry.card?.takeIf { entry.message } ?: continue
            val sent = card.content as? TimelineContent.A
            val time = sent?.sentAt ?: card.createdAt
            if (time < file.createdAt) continue
            position = if (sent != null) index - 1 else index
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
    vm: CairnViewModel,
    entry: TimelineEntry,
    agent: String,
    rendering: MarkdownRendering? = null,
    /** While the agent works, its step in progress is shown by the working indicator instead. */
    hideRunning: Boolean = false,
    /** The run is still active, so its subagents may still report. */
    active: Boolean = false,
    /** The plan the agent still follows, if any. */
    livePlan: String? = null,
) {
    if (entry.files.isNotEmpty()) ArtifactStrip(vm, entry.files)
    else if (entry.agents.isNotEmpty()) SubagentTeam(entry.agents, active, identity = entry.key)
    else if (entry.message && entry.card != null)
        CompositionLocalProvider(LocalMarkdownRendering provides rendering) {
            val card = entry.card
            val via = (card.content as? TimelineContent.B)?.via
            MessageRow(
                vm,
                card.userMessage,
                card.messageText(),
                (card.content as? TimelineContent.A)?.attachments.orEmpty(),
                card.createdAt,
                if (via.isNullOrBlank()) agent else "$agent · via $via",
            )
        }
    else if (entry.wait != null) WaitCard(entry.wait)
    else if (entry.plan != null) PlanCard(entry.plan, live = active && livePlan == entry.key)
    else if (entry.planUpdate != null) PlanUpdateRow(entry.planUpdate)
    else if (entry.compaction != null) ContextCompactionRow(entry.compaction)
    else if (entry.notice)
        (entry.card?.content as? TimelineContent.C)?.let { ChatNotice(it.notice) }
    else {
        // The run log presents a lifecycle event through its card, never its payload.
        val presentations =
            remember(entry.events, entry.notices, entry.card) {
                val events =
                    entry.events.map { event ->
                        entry.notices[event.id]?.let { presentNotice(it, event) }
                            ?: presentActivity(event)
                    }
                val notice = (entry.card?.content as? TimelineContent.C)?.notice
                if (notice == null) events else events + presentNotice(notice)
            }
        AgentActions(entry.key, presentations, hideRunning)
    }
}

/** A person's message or the agent's reply, from its timeline card. */
@Composable
fun MessageRow(
    vm: CairnViewModel,
    user: Boolean,
    content: String,
    attachments: List<ChatAttachment>,
    createdAt: Long,
    agent: String = "Cairn",
) {
    run {
        val time =
            java.text
                .SimpleDateFormat(
                    "HH:mm",
                    androidx.compose.ui.platform.LocalConfiguration.current.locales[0],
                )
                .format(java.util.Date(createdAt))
        BoxWithConstraints(
            Modifier.fillMaxWidth(),
            contentAlignment = if (user) Alignment.CenterEnd else Alignment.CenterStart,
        ) {
            // Like the web and other chat apps, a message's files stand above its bubble.
            Column(
                if (user) Modifier.widthIn(max = maxWidth * 0.9f) else Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.End,
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                if (attachments.isNotEmpty()) MessageAttachments(vm, attachments)
                if (!user || content.isNotBlank() || attachments.isEmpty())
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
                        modifier = if (user) Modifier else Modifier.fillMaxWidth(),
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
                                        time,
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
                                    time,
                                    Modifier.align(Alignment.End),
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                        }
                    }
                else
                    Text(
                        time,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
            }
        }
    }
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
