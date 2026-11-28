package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.viewModelScope
import build.cairn.app.data.*
import build.cairn.contracts.ReadRunHistoryInput
import build.cairn.contracts.TimelineContent
import build.cairn.contracts.readRunHistory
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.transformWhile
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.*

/** How long a revalidation of a saved conversation may take before the reader is told. */
internal const val SLOW_UPDATE_MILLIS = 1000L

@Composable
fun rememberLive(
    vm: CairnViewModel,
    workspace: Workspace,
    path: String,
    streaming: Boolean = true,
    /**
     * Whether a saved conversation still matches what the server holds. Every saved copy shows at
     * once; an outdated one says it is updating until the stream completes it, as on the web.
     */
    current: (CachedHistory) -> Boolean = { true },
): LiveSnapshot {
    val owner = LocalLifecycleOwner.current
    val enabled = workspace.session.authenticated && !workspace.signingOut
    val api = if (enabled) vm.api else null
    var value by remember(path, api) { mutableStateOf(LiveSnapshot()) }
    var reconnectGeneration by remember(path, api) { mutableIntStateOf(0) }
    val session = remember(path, api) { LiveSession() }
    // The saved copy shown until the stream completes it. Retained pager pages become previews and
    // then selected again without being restored: their latest snapshot is the copy. Whether it is
    // outdated is decided again on every composition, so a Fil read after the copy still flags it.
    var savedCopy by
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
            mutableStateOf(saved?.takeIf { streaming })
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

    LaunchedEffect(path, api, owner, streaming, reconnectGeneration) {
        if (enabled && api != null) {
            var cacheGeneration = vm.historyCache.generation
            val key = vm.historyCache.key(api.cacheScope, api.csrf, path)
            // A resumed session is already at least as recent as its cache.
            if (session.path != path)
                vm.historyCache.read(key)?.let {
                    session.restore(it, path, api.streamGeneration.get())
                    savedCopy = it.takeIf { streaming }
                    value = session.snapshot
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
                                // The saved copy shows until the stream's own state replaces it.
                                // Only a snapshot the stream delivered carries the server's
                                // revision: a saved copy, or a reconnect that kept its state, may
                                // carry the trash revision an earlier version followed.
                                if (it !== replay && it.status == LIVE_STATUS)
                                    vm.acceptCacheRevision(it.state?.historyRevision)
                                cacheGeneration = vm.historyCache.generation
                                if (!it.catchingUp || it.httpStatus != null) {
                                    if (it.history != value.history || it.epoch != value.epoch)
                                        olderError = null
                                    // A conversation that leaves the list was deleted or is no
                                    // longer shared: its saved copy goes too, as on the web. The
                                    // cache lock never delays the list on screen, and leaving the
                                    // screen does not cancel the removal.
                                    val left =
                                        if (path == "/chats/stream")
                                            leftChatPaths(value.state?.chats, it.state?.chats)
                                        else emptyList()
                                    value = pages.display(it)
                                    if (left.isNotEmpty())
                                        vm.viewModelScope.launch {
                                            for (saved in left) vm.historyCache.forget(
                                                vm.historyCache.key(
                                                    api.cacheScope,
                                                    api.csrf,
                                                    saved,
                                                )
                                            )
                                            vm.historyCache.removeRunLogs(api.cacheScope, api.csrf)
                                        }
                                    // Only the server's own complete state replaces the copy, as
                                    // on the web; a reconnect keeps saying it is updating.
                                    if (it !== replay && it.status == LIVE_STATUS) savedCopy = null
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
    // An outdated copy says it is updating, even when it came from an earlier live snapshot.
    val stale = savedCopy?.let { !current(it) } == true
    val visible =
        when {
            !stale -> value
            value.status == LIVE_STATUS -> value.copy(stale = true, status = UPDATING_STATUS)
            else -> value.copy(stale = true)
        }
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

                        val large = api.readLargeTimeline(path, page.largeEntries.orEmpty())
                        val events = api.readLargeEvents(path, page.events)
                        val applied =
                            pages.apply(
                                page.copy(entries = page.entries + large, events = events),
                                value,
                                requested,
                            )
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
        reconnect = { reconnectGeneration++ },
    )
}

/**
 * Inverted infinite scroll: prepend older pages ahead of the reader, several screens before the
 * start, and let Compose keep the first visible row in place by its key. A scrolled history never
 * gets a requested position, so a drag or fling in progress is never interrupted.
 */
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
    /** Queues the implementation of this proposed plan, when it is the latest one. */
    implement: (() -> Unit)? = null,
    canImplement: Boolean = false,
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
                planRequest = (card.content as? TimelineContent.A)?.plan == true,
                itemId = (card.content as? TimelineContent.B)?.itemId.orEmpty(),
                firstEvent = card.position,
                lastEvent = card.source,
            )
        }
    else if (entry.proposal) ProposedPlanCard(entry.events.single(), implement, canImplement)
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
    planRequest: Boolean = false,
    itemId: String = "",
    firstEvent: Long = 0,
    lastEvent: Long = firstEvent,
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
                            val side = if (user) null else LocalSideQuestions.current
                            if (!user)
                                Row(
                                    Modifier.fillMaxWidth(),
                                    horizontalArrangement = Arrangement.SpaceBetween,
                                    verticalAlignment = Alignment.CenterVertically,
                                ) {
                                    Text(
                                        agent,
                                        Modifier.weight(1f),
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                    val prefix = "event:$itemId:"
                                    val asked =
                                        (side?.counts?.get("item:$itemId") ?: 0) +
                                            side?.counts.orEmpty().entries.sumOf { (key, count) ->
                                                val id =
                                                    key.removePrefix(prefix)
                                                        .takeIf { key.startsWith(prefix) }
                                                        ?.toLongOrNull()
                                                if (id != null && id in firstEvent..lastEvent) count
                                                else 0
                                            }
                                    // The question count comes and goes with the reply's questions;
                                    // it never changes the line's height, so the history does not
                                    // move.
                                    if (side != null && itemId.isNotEmpty() && asked > 0)
                                        Box(Modifier.withoutLineHeight()) {
                                            val label =
                                                if (asked == 1)
                                                    "Voir 1 question à côté sur cette réponse"
                                                else
                                                    "Voir $asked questions à côté sur cette réponse"
                                            TextButton(
                                                onClick = {
                                                    side.show(itemId, firstEvent, lastEvent)
                                                },
                                                modifier =
                                                    Modifier.semantics {
                                                        contentDescription = label
                                                    },
                                                contentPadding = PaddingValues(horizontal = 8.dp),
                                            ) {
                                                Icon(
                                                    CairnIcons.SideQuestion,
                                                    null,
                                                    Modifier.size(14.dp),
                                                )
                                                Spacer(Modifier.width(4.dp))
                                                Text(
                                                    "$asked",
                                                    style = MaterialTheme.typography.labelSmall,
                                                )
                                            }
                                        }
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
                            else
                                CompositionLocalProvider(
                                    LocalSideQuestionAsk provides
                                        side
                                            ?.takeIf { it.canAsk && itemId.isNotEmpty() }
                                            ?.let { actions ->
                                                { quote -> actions.ask(itemId, quote, firstEvent) }
                                            }
                                ) {
                                    Markdown(content)
                                }
                            if (user)
                                Text(
                                    if (planRequest) "Plan · $time" else time,
                                    Modifier.align(Alignment.End),
                                    style = MaterialTheme.typography.labelSmall,
                                    color =
                                        if (planRequest) MaterialTheme.colorScheme.primary
                                        else MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                        }
                    }
                else
                    Text(
                        if (planRequest) "Plan · $time" else time,
                        style = MaterialTheme.typography.labelSmall,
                        color =
                            if (planRequest) MaterialTheme.colorScheme.primary
                            else MaterialTheme.colorScheme.onSurfaceVariant,
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
