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
import build.cairn.app.data.*
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.*

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
