package build.cairn.app.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.tween
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.animation.slideInVertically
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.colorResource
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import build.cairn.app.R
import build.cairn.app.data.*
import build.cairn.contracts.MissionStateInput
import build.cairn.contracts.RunMissionInput
import build.cairn.contracts.runMission
import build.cairn.contracts.setMissionState
import kotlinx.coroutines.delay

internal enum class FeedKind {
    QUESTION,
    FAILED_CHAT,
    RECONNECT,
    FAILED_TASK,
    REVIEW_TASK,
    /** Finished work the user has not read yet. */
    UNREAD_CHAT,
    UNREAD_TASK,
    RUNNING_CHAT,
    RUNNING_TASK,
    WAITING_CHAT,
    /** A mission run that scheduled its continuation. */
    WAITING_TASK,
    /** A blocked mission snoozed until [FeedItem.wakeAt]. */
    SNOOZED_TASK,
    /** A blocked mission that waits on someone else, or that the user dismissed. */
    SET_ASIDE_TASK,
    CHAT,
}

/**
 * Why a blocked mission sits in Récents rather than Pour vous: it waits on the agent work that
 * started it, on an outside system, or the user dismissed it.
 */
internal enum class SetAside {
    REQUESTER,
    EXTERNAL,
    DISMISSED,
}

/** The conversation or mission that started a mission, while it still exists. */
internal data class FeedRequester(val title: String, val chatId: String?, val runId: String?)

internal data class FeedItem(
    val key: String,
    val kind: FeedKind,
    val title: String,
    val subtitle: String,
    val agent: String,
    val agentKey: String,
    val stamp: Long = 0,
    val startedAt: Long? = null,
    val chatId: String? = null,
    val runId: String? = null,
    val taskId: String? = null,
    /** The agent is running now (not queued): the row animates. */
    val working: Boolean = false,
    /** A waiting conversation's deadline: when it resumes, or at the latest for a condition. */
    val wakeAt: Long? = null,
    /** It waits for a condition, so it may resume before [wakeAt]. */
    val condition: Boolean = false,
    /** Its result already arrived: it resumes as soon as capacity allows. */
    val resolved: Boolean = false,
    /** The opening of the agent's final message, for finished work not read yet. */
    val preview: String? = null,
    /** A mission's outcome that is not complete: what its actions apply to. */
    val outcome: String? = null,
    val setAside: SetAside? = null,
    val requester: FeedRequester? = null,
    /** The coding agent of its latest turn. */
    val harness: String? = null,
    val statusLabel: String = subtitle,
    val project: String? = null,
    val unread: Boolean = kind == FeedKind.UNREAD_CHAT || kind == FeedKind.UNREAD_TASK,
    val readAt: Long = stamp,
    /** It finished as expected: its row shows no status label, only a quiet mark. */
    val finished: Boolean = false,
) {
    /** Only a blocked outcome can be dismissed; input the agent asked for can only be snoozed. */
    val dismissible
        get() = kind == FeedKind.REVIEW_TASK && outcome == "blocked"

    /** What reading this item marks, with the end of the work it reads. */
    val read
        get() = if (taskId != null) "task:$taskId" to readAt else "chat:$chatId" to readAt
}

internal data class Feed(
    val forYou: List<FeedItem>,
    val running: List<FeedItem>,
    val recent: List<FeedItem>,
    /** Conversations and missions that resume on their own, soonest first. */
    val waiting: List<FeedItem> = emptyList(),
) {
    val empty
        get() = forYou.isEmpty() && running.isEmpty() && waiting.isEmpty() && recent.isEmpty()

    /** Finished work waiting to be read, below what needs an answer or a decision. */
    val all
        get() = (forYou + running + waiting + recent).sortedByDescending { it.stamp }

    val unread
        get() = all.filter { it.unread }

    val ongoing
        get() = all.filter {
            it.kind in
                setOf(
                    FeedKind.QUESTION,
                    FeedKind.RECONNECT,
                    FeedKind.REVIEW_TASK,
                    FeedKind.RUNNING_CHAT,
                    FeedKind.RUNNING_TASK,
                    FeedKind.WAITING_CHAT,
                    FeedKind.WAITING_TASK,
                    FeedKind.SNOOZED_TASK,
                ) || (it.kind == FeedKind.SET_ASIDE_TASK && it.setAside != SetAside.DISMISSED)
        }
}

private fun context(agent: String, project: String?) =
    listOfNotNull(agent.ifBlank { null }, project?.ifBlank { null }).joinToString(" · ")

/** 9:00 tomorrow, local time: when a snoozed mission asks again. */
internal fun tomorrowMorning(
    now: Long = System.currentTimeMillis(),
    zone: java.time.ZoneId = java.time.ZoneId.systemDefault(),
): Long =
    java.time.Instant.ofEpochMilli(now)
        .atZone(zone)
        .toLocalDate()
        .plusDays(1)
        .atTime(9, 0)
        .atZone(zone)
        .toInstant()
        .toEpochMilli()

/**
 * One home for conversations and missions, grouped by what they ask of the user: things that need
 * an answer or decision, finished work not read yet, work in progress, waiting conversations, then
 * everything else by recency. [read] holds local marks until the server reflects them.
 *
 * A blocked mission asks the user only when it waits on them: one waiting on the agent work that
 * started it or on an outside system, or one the user dismissed, sits in Récents. A snoozed one
 * waits until [now] reaches its time.
 */
internal fun buildFeed(
    chats: List<Chat>,
    activity: List<Run>,
    tasks: List<Task>,
    read: Map<String, Long> = emptyMap(),
    now: Long = System.currentTimeMillis(),
): Feed {
    fun unreadNow(key: String, finishedAt: Long?) =
        (read[key] ?: Long.MIN_VALUE) < (finishedAt ?: 0)
    val forYou = mutableListOf<FeedItem>()
    val toRead = mutableListOf<FeedItem>()
    val running = mutableListOf<FeedItem>()
    val waiting = mutableListOf<FeedItem>()
    val recent = mutableListOf<FeedItem>()
    chats
        .sortedByDescending { it.updatedAt }
        .forEach { chat ->
            val agent = chat.agentName.ifBlank { "Agent" }
            fun item(kind: FeedKind, subtitle: String) =
                FeedItem(
                    "chat:${chat.id}",
                    kind,
                    chat.title,
                    subtitle,
                    agent,
                    chat.agentId,
                    chat.updatedAt,
                    chat.run?.startedAt,
                    chatId = chat.id,
                    runId = chat.runId,
                    // A delegated conversation names the conversation that delegated it.
                    requester =
                        chat.requester?.let {
                            FeedRequester(
                                it.title.ifBlank { "Nouvelle conversation" },
                                it.chatId,
                                null,
                            )
                        },
                    harness = chat.provider,
                    project = chat.projectName,
                    statusLabel =
                        when (kind) {
                            FeedKind.QUESTION -> "Répondre"
                            FeedKind.RECONNECT -> "Connexion requise"
                            FeedKind.RUNNING_CHAT ->
                                if (chat.exclusiveSuspended) "Suspendue"
                                else if (chat.status == "queued") "En attente" else "En cours"
                            FeedKind.WAITING_CHAT -> "Reprise prévue"
                            else -> if (chat.paused) "En pause" else statusLabel(chat.status)
                        },
                    readAt = chat.readAt ?: chat.finishedAt ?: chat.updatedAt,
                    unread =
                        chat.unread &&
                            unreadNow("chat:${chat.id}", chat.readAt ?: chat.finishedAt) &&
                            (chat.requestedBy == null || chat.pendingQuestions > 0),
                )
            val wait = chatWaitNotice(chat.run)
            when {
                chat.pendingQuestions > 0 ->
                    forYou +=
                        item(
                            FeedKind.QUESTION,
                            if (chat.pendingQuestions == 1L) "Une question vous attend"
                            else "${chat.pendingQuestions} questions vous attendent",
                        )
                wait?.account != null ->
                    forYou +=
                        item(FeedKind.RECONNECT, "Compte ${wait.account} requis pour continuer")
                chat.status in setOf("failed", "interrupted") ->
                    forYou +=
                        item(
                            FeedKind.FAILED_CHAT,
                            chat.error?.lineSequence()?.firstOrNull()?.ifBlank { null }
                                ?: statusLabel(chat.status),
                        )
                !chat.paused &&
                    !chat.exclusiveSuspended &&
                    isQuotaWait(
                        chat.status,
                        chat.accountWaitReason ?: chat.run?.accountWaitReason,
                        chat.accountRequired ?: chat.run?.accountRequired,
                    ) -> waiting += item(FeedKind.WAITING_CHAT, "En attente de quota")
                !chat.paused && chat.status in setOf("running", "queued") ->
                    running +=
                        item(
                                FeedKind.RUNNING_CHAT,
                                if (chat.exclusiveSuspended)
                                    "Suspendue (exclusivité) · ${context(agent, chat.projectName)}"
                                else if (chat.status == "queued")
                                    "En attente · ${context(agent, chat.projectName)}"
                                else context(agent, chat.projectName),
                            )
                            .copy(working = chat.status == "running" && !chat.exclusiveSuspended)
                !chat.paused && chat.wakeup != null && chat.status == "succeeded" ->
                    waiting +=
                        item(
                                FeedKind.WAITING_CHAT,
                                listOfNotNull(
                                        waitTarget(chat.wakeup).short,
                                        chat.projectName?.ifBlank { null },
                                    )
                                    .joinToString(" · "),
                            )
                            .copy(
                                wakeAt = chat.wakeup.wakeAt,
                                condition = chat.wakeup.condition != null,
                                // Recorded by plugin condition polling; another run's result is
                                // only computed
                                // at resumption.
                                resolved = chat.wakeup.observation != null,
                            )
                chat.unread &&
                    !chat.paused &&
                    chat.requestedBy == null &&
                    unreadNow("chat:${chat.id}", chat.readAt ?: chat.finishedAt) ->
                    toRead +=
                        item(FeedKind.UNREAD_CHAT, context(agent, chat.projectName))
                            .copy(
                                stamp = chat.finishedAt ?: chat.updatedAt,
                                preview = chat.preview,
                                finished = chat.status == "succeeded",
                            )
                else ->
                    recent +=
                        item(
                                FeedKind.CHAT,
                                if (chat.paused) "En pause · ${context(agent, chat.projectName)}"
                                else if (chat.status == "succeeded")
                                    "Terminée · ${context(agent, chat.projectName)}"
                                else context(agent, chat.projectName),
                            )
                            .copy(finished = !chat.paused && chat.status == "succeeded")
            }
        }
    val names = tasks.associateBy { it.id }
    val missionRuns = activity.filter { it.trigger != "chat" }.associateBy { it.taskId }
    // The work that started a mission, while it is still there to act.
    fun requesterOf(run: Run): FeedRequester? {
        val requested = run.requestedBy ?: return null
        chats
            .find { it.id == requested.chatId }
            ?.let {
                return FeedRequester(it.title, it.id, null)
            }
        val mission = names[requested.taskId]?.takeIf { !it.archived } ?: return null
        val missionRun = missionRuns[mission.id] ?: return null
        return FeedRequester(mission.name, null, missionRun.id)
    }
    activity
        .filter { it.trigger != "chat" && it.taskId.isNotBlank() }
        .sortedByDescending { it.finishedAt ?: it.startedAt ?: it.createdAt }
        .forEach { run ->
            val task = names[run.taskId]
            if (task?.archived == true) return@forEach
            val agent =
                run.agentName.ifBlank { run.snapshot?.agent?.name.orEmpty() }.ifBlank { "Agent" }
            val agentKey = run.snapshot?.agent?.id ?: task?.agentId.orEmpty()
            fun item(kind: FeedKind, subtitle: String) =
                FeedItem(
                    "run:${run.id}",
                    kind,
                    task?.name ?: run.title,
                    subtitle,
                    agent,
                    agentKey,
                    run.finishedAt ?: run.startedAt ?: run.createdAt,
                    run.startedAt,
                    runId = run.id,
                    taskId = run.taskId,
                    harness = run.provider,
                    statusLabel =
                        when (kind) {
                            FeedKind.REVIEW_TASK ->
                                if (run.outcome?.status == "needs_input") "Répondre" else "Bloquée"
                            FeedKind.RUNNING_TASK ->
                                if (run.suspendedByExclusive != null) "Suspendue"
                                else if (run.status == "queued") "En attente" else "En cours"
                            FeedKind.WAITING_TASK -> "Reprise prévue"
                            else -> statusLabel(run.status)
                        },
                    readAt = run.finishedAt ?: run.createdAt,
                    unread = run.unread && unreadNow("task:${run.taskId}", run.finishedAt),
                )
            when {
                // A mission that scheduled its continuation resumes on its own, like a chat.
                run.status == "waiting" && run.suspendedByExclusive == null ->
                    waiting +=
                        item(
                                FeedKind.WAITING_TASK,
                                run.wakeup?.reason?.ifBlank { null }
                                    ?: "Mission en attente de reprise",
                            )
                            .copy(
                                wakeAt = run.wakeup?.wakeAt,
                                condition = run.wakeup?.condition != null,
                                resolved = run.wakeup?.observation != null,
                            )
                run.suspendedByExclusive == null &&
                    isQuotaWait(run.status, run.accountWaitReason, run.accountRequired) ->
                    waiting += item(FeedKind.WAITING_TASK, "En attente de quota")
                run.active ->
                    running +=
                        item(
                                FeedKind.RUNNING_TASK,
                                if (run.suspendedByExclusive != null)
                                    "Suspendue (exclusivité) · Mission · $agent"
                                else if (run.status == "queued") "Mission en attente · $agent"
                                else "Mission · $agent",
                            )
                            .copy(
                                working =
                                    run.status == "running" && run.suspendedByExclusive == null
                            )
                run.status in setOf("failed", "interrupted") ->
                    forYou +=
                        item(
                            FeedKind.FAILED_TASK,
                            "${statusLabel(run.status)} · ${shortStamp(run.finishedAt ?: run.createdAt)}",
                        )
                run.status == "succeeded" &&
                    run.outcome != null &&
                    run.outcome.status != "completed" -> {
                    val label = outcomeLabel(run.outcome)
                    val review =
                        item(FeedKind.REVIEW_TASK, label).copy(outcome = run.outcome.status)
                    fun setAside(reason: SetAside, subtitle: String) =
                        review.copy(
                            kind = FeedKind.SET_ASIDE_TASK,
                            setAside = reason,
                            subtitle = "$label · $subtitle",
                        )
                    // A requester that is gone leaves the user to act.
                    val requester =
                        if (run.outcome.waitingOn == "requester") requesterOf(run) else null
                    val until = run.dismissal?.until
                    when {
                        requester != null ->
                            recent +=
                                setAside(SetAside.REQUESTER, "En attente de ${requester.title}")
                                    .copy(requester = requester)
                        run.outcome.waitingOn == "external" ->
                            recent +=
                                setAside(SetAside.EXTERNAL, "En attente d’une vérification externe")
                        run.dismissal != null && until == null && run.outcome.status == "blocked" ->
                            recent += setAside(SetAside.DISMISSED, "Écartée")
                        until != null && until > now ->
                            waiting +=
                                review.copy(
                                    kind = FeedKind.SNOOZED_TASK,
                                    subtitle = "Rappel prévu · $label",
                                    wakeAt = until,
                                )
                        else -> forYou += review
                    }
                }
                run.status == "succeeded" &&
                    run.unread &&
                    unreadNow("task:${run.taskId}", run.finishedAt) ->
                    toRead +=
                        item(FeedKind.UNREAD_TASK, "Mission terminée · $agent")
                            .copy(finished = true)
                else ->
                    recent +=
                        item(FeedKind.UNREAD_TASK, "Mission terminée · $agent")
                            .copy(unread = false, finished = run.status == "succeeded")
            }
        }
    return Feed(
        forYou + toRead.sortedByDescending { it.stamp },
        running.sortedBy { it.startedAt ?: Long.MAX_VALUE },
        recent.sortedByDescending { it.stamp },
        // A wait whose result arrived resumes first, before any deadline.
        waiting.sortedWith(compareBy({ !it.resolved }, { it.wakeAt ?: Long.MAX_VALUE })),
    )
}

/**
 * Rows in reading order, each delegated conversation or mission right below the work that started
 * it when both are listed. One that started without its requester in the list stays in place and
 * names it instead. The second value tells whether the row is nested.
 */
internal fun threadFeed(items: List<FeedItem>): List<Pair<FeedItem, Boolean>> {
    val byChat = items.filter { it.chatId != null }.associateBy { it.chatId }
    val byRun = items.filter { it.runId != null }.associateBy { it.runId }
    val childrenOf = mutableMapOf<String, MutableList<FeedItem>>()
    val roots = mutableListOf<FeedItem>()
    items.forEach { item ->
        val parent =
            item.requester?.let { requester ->
                requester.chatId?.let { byChat[it] } ?: requester.runId?.let { byRun[it] }
            }
        if (parent != null && parent.key != item.key)
            childrenOf.getOrPut(parent.key) { mutableListOf() } += item
        else roots += item
    }

    val rows = mutableListOf<Pair<FeedItem, Boolean>>()
    val placed = mutableSetOf<String>()
    fun place(item: FeedItem, nested: Boolean) {
        if (!placed.add(item.key)) return
        rows += item to nested
        childrenOf[item.key]?.forEach { place(it, true) }
    }
    roots.forEach { place(it, false) }
    // A requester cycle has no root: keep those rows rather than losing them.
    items.forEach { place(it, false) }
    return rows
}

/** The Fil in reading order, one key per item: what a swipe between pages moves through. */
internal fun Feed.keys() = all.map { it.key }

/** The shell and its pages share one conversation stream and one mission poll. */
internal data class FilSnapshot(val conversations: LiveSnapshot, val activity: List<Run>?)

internal val LocalFilSnapshot = compositionLocalOf<FilSnapshot?> { null }

@Composable
internal fun rememberFilConversations(vm: CairnViewModel, state: Workspace): LiveSnapshot =
    LocalFilSnapshot.current?.conversations ?: rememberLive(vm, state, "/chats/stream")

/** The latest run of each mission, polled while the Fil or its pager shows; null until loaded. */
@Composable
internal fun rememberFilActivity(vm: CairnViewModel): List<Run>? {
    LocalFilSnapshot.current?.let {
        return it.activity
    }
    var activity by remember { mutableStateOf<List<Run>?>(null) }
    Poll("fil-activity", 5000) {
        try {
            activity = vm.missionActivity()
        } catch (e: Exception) {
            vm.report(e)
        }
    }
    return activity?.let { withLocalDismissals(it, vm.localDismissals) }
}

/** Outcomes set aside on this device show at once, before the next poll confirms them. */
internal fun withLocalDismissals(activity: List<Run>, local: Map<String, OutcomeDismissal?>) =
    if (local.isEmpty()) activity
    else
        activity.map { run ->
            if (run.id in local) run.copy(dismissal = local[run.id]) else run
        }

internal fun feedSummary(feed: Feed): String {
    val parts = mutableListOf<String>()
    if (feed.running.isNotEmpty())
        parts +=
            if (feed.running.size == 1) "1 agent au travail"
            else "${feed.running.size} agents au travail"
    val need = feed.forYou.count { !it.unread }
    if (need > 0) parts += if (need == 1) "1 élément pour vous" else "$need éléments pour vous"
    if (feed.unread.isNotEmpty())
        parts +=
            if (feed.unread.size == 1) "1 résultat à lire"
            else "${feed.unread.size} résultats à lire"
    if (feed.waiting.isNotEmpty())
        parts +=
            if (feed.waiting.size == 1) "1 reprise prévue"
            else "${feed.waiting.size} reprises prévues"
    return parts.joinToString(" · ").ifBlank { "Tout est calme." }
}

internal fun greeting(hour: Int = java.time.LocalTime.now().hour) =
    if (hour in 5..17) "Bonjour." else "Bonsoir."

@Composable
fun FilScreen(
    vm: CairnViewModel,
    state: Workspace,
    openChat: (String) -> Unit,
    openRun: (String) -> Unit,
    openConnections: () -> Unit,
    search: () -> Unit,
    openAccount: () -> Unit = {},
    openSettings: () -> Unit = openAccount,
) {
    val live = rememberFilConversations(vm, state)
    var connection by rememberSaveable { mutableStateOf(false) }
    if (connection)
        ConnectionSheet(vm, state, close = { connection = false }, openSettings = openSettings)
    val activity = rememberFilActivity(vm).orEmpty()
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(30_000)
            now = System.currentTimeMillis()
        }
    }
    val trash = rememberConversationTrash(vm)
    val chats = live.state?.chats?.filter { it.id !in trash.hidden }
    val feed = buildFeed(chats.orEmpty(), activity, state.tasks, vm.localReads, now)
    var selectedView by rememberSaveable { mutableIntStateOf(0) }
    var filter by rememberSaveable { mutableStateOf("") }
    // The filter opens on demand and stays open while it holds a query.
    var searchOpen by rememberSaveable { mutableStateOf(false) }
    val searching = searchOpen || filter.isNotEmpty()
    val views = listOf(feed.unread, feed.ongoing, feed.all)
    val query = filter.trim()
    val visible =
        views[selectedView].filter {
            query.isEmpty() ||
                listOfNotNull(it.title, it.subtitle, it.agent, it.project).any { value ->
                    value.contains(query, ignoreCase = true)
                }
        }
    fun retry(item: FeedItem) {
        val task = item.taskId ?: return
        vm.perform { openRun(api.runMission(RunMissionInput(task)).id) }
    }
    fun setAside(item: FeedItem, until: Long? = null) {
        vm.setAside(item.runId ?: return, item.taskId ?: return, item.stamp, until)
    }
    fun archive(item: FeedItem) {
        val task = state.tasks.find { it.id == item.taskId } ?: return
        vm.perform {
            api.setMissionState(
                MissionStateInput(
                    taskId = task.id,
                    revision = task.revision,
                    enabled = false,
                    archived = true,
                )
            )
            refresh()
            notify("Modifications enregistrées")
        }
    }
    fun open(item: FeedItem) {
        when {
            item.kind == FeedKind.RECONNECT -> openConnections()
            item.chatId != null -> openChat(item.chatId)
            item.runId != null -> openRun(item.runId)
        }
    }
    LazyColumn(
        Modifier.fillMaxSize().testTag("fil"),
        contentPadding = PaddingValues(bottom = 24.dp),
    ) {
        item(key = "header") {
            Column(Modifier.widthIn(max = 760.dp).padding(horizontal = 20.dp)) {
                InstallationHeader(
                    state,
                    openConnection = { connection = true },
                    search = search,
                    account = openAccount,
                )
                Spacer(Modifier.height(12.dp))
                FilHeader(
                    working = feed.running.filter { it.working }.distinctBy { it.agentKey },
                    unread = feed.unread.isNotEmpty(),
                    searching = searching,
                    toggleSearch = {
                        if (searching) {
                            filter = ""
                            searchOpen = false
                        } else searchOpen = true
                    },
                    showOngoing = { selectedView = 1 },
                    readAll = { vm.markRead(feed.unread.map { it.read }) },
                )
                FilSearch(
                    searching,
                    filter,
                    { filter = it },
                    close = {
                        filter = ""
                        searchOpen = false
                    },
                )
                FilViews(
                    listOf("À lire", "En cours", "Tout"),
                    listOf(views[0].size, views[1].size, null),
                    selectedView,
                ) {
                    selectedView = it
                }
                if (chats == null)
                    LinearProgressIndicator(Modifier.fillMaxWidth().padding(top = 12.dp))
                live.error?.let {
                    Text(it, Modifier.padding(top = 8.dp), color = MaterialTheme.colorScheme.error)
                }
            }
        }
        if (chats != null && feed.empty)
            item(key = "empty") {
                Box(Modifier.padding(20.dp)) {
                    Empty(
                        "Aucune conversation",
                        "Appuyez sur + pour confier une mission à un agent.",
                    )
                }
            }
        if (chats != null && visible.isEmpty() && !feed.empty)
            item(key = "view-empty") {
                Text(
                    if (filter.isNotBlank()) "Aucun résultat"
                    else if (selectedView == 0) "Tout est lu." else "Aucune tâche en cours.",
                    Modifier.padding(20.dp),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        items(threadFeed(visible), key = { it.first.key }) { (item, nested) ->
            FeedRow(
                item,
                { open(item) },
                onDelete = item.chatId?.let { id -> { trash.trash(id) } },
                now = now,
                nested = nested,
            ) {
                ForYouAction(
                    item,
                    now,
                    !state.busy,
                    { open(item) },
                    { retry(item) },
                    { vm.markRead(listOf(item.read)) },
                    dismiss = { setAside(item) },
                    snooze = { setAside(item, tomorrowMorning(now)) },
                    archive = { archive(item) },
                )
            }
        }
    }
}

/** The Fil's title, the agents at work, and its filter and read-all actions. */
@Composable
private fun FilHeader(
    working: List<FeedItem>,
    unread: Boolean,
    searching: Boolean,
    toggleSearch: () -> Unit,
    showOngoing: () -> Unit,
    readAll: () -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
            "Fil",
            style = MaterialTheme.typography.titleLarge,
            fontWeight = FontWeight.Bold,
        )
        if (working.isNotEmpty()) {
            Spacer(Modifier.width(10.dp))
            Surface(
                onClick = showOngoing,
                shape = RoundedCornerShape(50),
                color = MaterialTheme.colorScheme.primaryContainer,
                contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
                modifier = Modifier.testTag("fil-at-work"),
            ) {
                Row(
                    Modifier.padding(start = 3.dp, end = 10.dp, top = 3.dp, bottom = 3.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    working.take(3).forEachIndexed { index, item ->
                        Box(
                            Modifier.offset(x = (-6 * index).dp)
                                .background(
                                    MaterialTheme.colorScheme.primaryContainer,
                                    RoundedCornerShape(7.dp),
                                )
                                .padding(1.5.dp)
                        ) {
                            AgentAvatar(item.agent, item.agentKey.ifBlank { item.agent }, 20.dp)
                        }
                    }
                    Text(
                        "${working.size} au travail",
                        Modifier.offset(x = (-6 * (minOf(working.size, 3) - 1)).dp)
                            .padding(start = 6.dp),
                        style = MaterialTheme.typography.labelMedium,
                        fontWeight = FontWeight.SemiBold,
                    )
                }
            }
        }
        Spacer(Modifier.weight(1f))
        RoundAction(
            "Filtrer le fil",
            CairnIcons.Search,
            Modifier.testTag("fil-search"),
            container =
                if (searching) MaterialTheme.colorScheme.secondaryContainer
                else MaterialTheme.colorScheme.surface,
            size = 36.dp,
            onClick = toggleSearch,
        )
        if (unread) {
            Spacer(Modifier.width(6.dp))
            RoundAction(
                "Tout marquer comme lu",
                CairnIcons.Check,
                Modifier.testTag("read-all"),
                size = 36.dp,
                onClick = readAll,
            )
        }
    }
}

/** The filter field, which unfolds from the header when the user asks to filter. */
@Composable
private fun FilSearch(
    open: Boolean,
    value: String,
    change: (String) -> Unit,
    close: () -> Unit,
) {
    val focus = remember { FocusRequester() }
    AnimatedVisibility(
        open,
        enter =
            expandVertically(tween(220)) +
                fadeIn(tween(220)) +
                slideInVertically(tween(220)) { -it / 3 },
        exit = shrinkVertically(tween(180)) + fadeOut(tween(140)),
    ) {
        LaunchedEffect(Unit) { focus.requestFocus() }
        OutlinedTextField(
            value = value,
            onValueChange = change,
            modifier = Modifier.fillMaxWidth().padding(top = 12.dp).focusRequester(focus),
            placeholder = { Text("Titre, agent ou projet") },
            leadingIcon = { Icon(CairnIcons.Search, null, Modifier.size(18.dp)) },
            trailingIcon = {
                androidx.compose.material3.IconButton(onClick = close) {
                    Icon(CairnIcons.Close, "Fermer le filtre", Modifier.size(18.dp))
                }
            },
            shape = RoundedCornerShape(50),
            singleLine = true,
        )
    }
}

/** À lire / En cours / Tout as a segmented control whose selection slides between views. */
@Composable
private fun FilViews(
    labels: List<String>,
    counts: List<Int?>,
    selected: Int,
    select: (Int) -> Unit,
) {
    BoxWithConstraints(
        Modifier.fillMaxWidth()
            .padding(top = 12.dp)
            .background(
                MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.6f),
                RoundedCornerShape(14.dp),
            )
            .padding(4.dp)
            .selectableGroup()
    ) {
        val width = maxWidth / labels.size
        val offset by animateDpAsState(width * selected, tween(220), label = "fil-view")
        val dark = MaterialTheme.colorScheme.background.luminance() < 0.5f
        Box(
            Modifier.offset(x = offset)
                .width(width)
                .height(36.dp)
                .shadow(1.dp, RoundedCornerShape(10.dp))
                .background(
                    if (dark) MaterialTheme.colorScheme.surfaceContainerHighest
                    else MaterialTheme.colorScheme.surface,
                    RoundedCornerShape(10.dp),
                )
                .border(
                    1.dp,
                    MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f),
                    RoundedCornerShape(10.dp),
                )
        )
        Row {
            labels.forEachIndexed { index, label ->
                val count = counts[index]?.takeIf { it > 0 }
                val accent = MaterialTheme.colorScheme.primary
                Box(
                    Modifier.weight(1f).height(36.dp).clip(RoundedCornerShape(10.dp)).selectable(
                        selected == index,
                        role = Role.Tab,
                    ) {
                        select(index)
                    },
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        buildAnnotatedString {
                            append(label)
                            if (count != null) {
                                append(" ")
                                withStyle(
                                    SpanStyle(
                                        color = if (index == 0) accent else Color.Unspecified,
                                        fontWeight = FontWeight.Bold,
                                    )
                                ) {
                                    append("$count")
                                }
                            }
                        },
                        style = MaterialTheme.typography.labelLarge,
                        color =
                            if (selected == index) MaterialTheme.colorScheme.onSurface
                            else MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

private fun androidx.compose.foundation.lazy.LazyListScope.section(
    label: String,
    items: List<FeedItem>,
    action: (@Composable () -> Unit)? = null,
    row: @Composable (FeedItem) -> Unit,
) {
    if (items.isEmpty()) return
    item(key = "section:$label") {
        Row(
            Modifier.widthIn(max = 760.dp)
                .fillMaxWidth()
                .padding(start = 20.dp, end = 12.dp, top = 24.dp, bottom = 4.dp)
                .heightIn(min = 32.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Eyebrow(label, Modifier.weight(1f).semantics { heading() })
            action?.invoke()
        }
    }
    items(items, key = { it.key }) { Box(Modifier.animateItem()) { row(it) } }
}

/**
 * What a "for you" row offers: answer, reconnect, retry, mark read, set a blocked mission aside, or
 * its time.
 */
@Composable
private fun ForYouAction(
    item: FeedItem,
    now: Long,
    enabled: Boolean,
    open: () -> Unit,
    retry: () -> Unit,
    markRead: () -> Unit,
    dismiss: () -> Unit = {},
    snooze: () -> Unit = {},
    archive: () -> Unit = {},
) {
    var menu by remember { mutableStateOf(false) }
    Box {
        RoundAction("Autres actions pour ${item.title}", CairnIcons.More, size = 36.dp) {
            menu = true
        }
        DropdownMenu(menu, { menu = false }) {
            if (item.unread)
                DropdownMenuItem(
                    text = { Text("Marquer comme lu") },
                    onClick = {
                        menu = false
                        markRead()
                    },
                )
            DropdownMenuItem(
                text = { Text("Ouvrir") },
                onClick = {
                    menu = false
                    open()
                },
            )
            if (item.dismissible)
                DropdownMenuItem(
                    text = { Text("Écarter le résultat") },
                    onClick = {
                        menu = false
                        dismiss()
                    },
                )
            if (item.kind == FeedKind.REVIEW_TASK)
                DropdownMenuItem(
                    text = { Text("Me le rappeler demain") },
                    onClick = {
                        menu = false
                        snooze()
                    },
                )
            if (item.taskId != null) {
                DropdownMenuItem(
                    text = { Text("Relancer") },
                    enabled = enabled,
                    onClick = {
                        menu = false
                        retry()
                    },
                )
                DropdownMenuItem(
                    text = { Text("Archiver la mission") },
                    enabled = enabled,
                    onClick = {
                        menu = false
                        archive()
                    },
                )
            }
        }
    }
}

@Composable
private fun Stamp(value: Long, now: Long) {
    Text(
        shortStamp(value, now),
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
internal fun CairnLogo() {
    Image(
        painter = painterResource(R.drawable.ic_cairn),
        contentDescription = null,
        modifier =
            Modifier.size(40.dp)
                .background(
                    CairnLightColors.primary,
                    RoundedCornerShape(10.dp),
                )
                .border(
                    1.dp,
                    colorResource(R.color.cairn_icon_outline),
                    RoundedCornerShape(10.dp),
                ),
    )
}

@Composable
internal fun Wordmark() {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        CairnLogo()
        Text(
            "Cairn",
            style = MaterialTheme.typography.headlineMedium,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.primary,
        )
    }
}

/** The agent's avatar with what the row asks for in its corner; finished work stays quiet. */
@Composable
private fun FilAvatar(item: FeedItem, size: androidx.compose.ui.unit.Dp) {
    Box {
        AgentAvatar(item.agent, item.agentKey.ifBlank { item.agent }, size)
        if (!item.finished)
            Box(
                Modifier.align(Alignment.BottomEnd)
                    .offset(5.dp, 4.dp)
                    .background(MaterialTheme.colorScheme.background, CircleShape)
                    .padding(1.dp)
            ) {
                StatusMark(item)
            }
    }
}

/** The row's status glyph, coloured like its label: quiet once the work finished as expected. */
@Composable
private fun StatusMark(item: FeedItem) {
    val (icon, tint) =
        when {
            item.kind == FeedKind.QUESTION ||
                (item.kind == FeedKind.REVIEW_TASK && item.outcome == "needs_input") ->
                CairnIcons.CircleAlert to signal.answer
            item.kind == FeedKind.RECONNECT -> CairnIcons.CircleAlert to signal.warning
            item.kind == FeedKind.FAILED_CHAT || item.kind == FeedKind.FAILED_TASK ->
                CairnIcons.CircleX to signal.attention
            item.kind == FeedKind.REVIEW_TASK -> CairnIcons.CirclePause to signal.warning
            item.kind == FeedKind.RUNNING_CHAT || item.kind == FeedKind.RUNNING_TASK ->
                CairnIcons.CircleProgress to MaterialTheme.colorScheme.primary
            item.kind == FeedKind.WAITING_CHAT ||
                item.kind == FeedKind.WAITING_TASK ||
                item.kind == FeedKind.SNOOZED_TASK ->
                CairnIcons.Clock to MaterialTheme.colorScheme.primary
            item.kind == FeedKind.SET_ASIDE_TASK ->
                CairnIcons.Hourglass to MaterialTheme.colorScheme.onSurfaceVariant
            item.finished ->
                (if (item.taskId != null) CairnIcons.Retry else CairnIcons.CircleCheck) to
                    MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.7f)
            item.kind == FeedKind.CHAT && item.statusLabel == "En pause" ->
                CairnIcons.CirclePause to MaterialTheme.colorScheme.onSurfaceVariant
            else -> CairnIcons.Circle to MaterialTheme.colorScheme.onSurfaceVariant
        }
    Icon(
        icon,
        contentDescription = item.statusLabel,
        modifier = Modifier.size(14.dp).testTag("fil-mark"),
        tint = tint,
    )
}

@Composable
private fun FeedRow(
    item: FeedItem,
    onClick: () -> Unit,
    onDelete: (() -> Unit)? = null,
    now: Long,
    nested: Boolean = false,
    trailing: @Composable () -> Unit,
) {
    val thread = MaterialTheme.colorScheme.outlineVariant
    // Where the text below the title starts: after the unread dot, the avatar and its gap.
    val inset = if (nested) 42.dp else 50.dp
    SwipeToTrashRow(enabled = onDelete != null, onTrash = { onDelete?.invoke() }) { swipe ->
        Surface(
            onClick = onClick,
            color = Color.Transparent,
            modifier =
                swipe
                    .fillMaxWidth()
                    .then(if (item.unread) Modifier.testTag("unread") else Modifier)
                    .then(if (nested) Modifier.testTag("fil-nested") else Modifier),
        ) {
            Row(
                Modifier.widthIn(max = 760.dp)
                    .heightIn(min = if (nested) 44.dp else 56.dp)
                    .padding(start = if (nested) 36.dp else 10.dp, end = 16.dp)
                    .then(
                        if (nested)
                            Modifier.drawBehind {
                                drawLine(
                                    thread,
                                    Offset(0f, 0f),
                                    Offset(0f, size.height),
                                    1.dp.toPx(),
                                )
                            }
                        else Modifier
                    )
                    .padding(start = if (nested) 8.dp else 0.dp, top = 8.dp, bottom = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Box(Modifier.width(12.dp)) {
                            if (item.unread)
                                Box(
                                    Modifier.size(6.dp)
                                        .background(
                                            MaterialTheme.colorScheme.primary,
                                            androidx.compose.foundation.shape.CircleShape,
                                        )
                                        .semantics { contentDescription = "Non lu" }
                                )
                        }
                        FilAvatar(item, if (nested) 20.dp else 28.dp)
                        Spacer(Modifier.width(10.dp))
                        Text(
                            item.title,
                            modifier = Modifier.weight(1f),
                            style = MaterialTheme.typography.titleSmall,
                            fontWeight =
                                if (item.unread) FontWeight.SemiBold else FontWeight.Medium,
                            color =
                                if (nested && !item.unread)
                                    MaterialTheme.colorScheme.onSurfaceVariant
                                else MaterialTheme.colorScheme.onSurface,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Spacer(Modifier.width(8.dp))
                        Stamp(item.stamp, now)
                    }
                    val needsAnswer =
                        item.kind == FeedKind.QUESTION ||
                            (item.kind == FeedKind.REVIEW_TASK && item.outcome == "needs_input")
                    val failed =
                        item.kind == FeedKind.FAILED_CHAT || item.kind == FeedKind.FAILED_TASK
                    val label = item.statusLabel
                    if (!nested || !item.finished)
                        Row(
                            Modifier.padding(start = inset, top = 2.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            if (!nested)
                                item.harness?.let {
                                    Box(
                                        Modifier.padding(end = 5.dp)
                                            .testTag("harness-$it")
                                            .semantics {
                                                contentDescription =
                                                    "Dernier harness : ${providerLabel(it)}"
                                            }
                                    ) {
                                        ProviderMark(it, 12.dp)
                                    }
                                }
                            if (!item.finished)
                                Text(
                                    label,
                                    style = MaterialTheme.typography.labelMedium,
                                    fontWeight = FontWeight.Medium,
                                    color =
                                        if (needsAnswer) signal.answer
                                        else if (failed) signal.attention
                                        else if (item.kind == FeedKind.REVIEW_TASK) signal.warning
                                        else MaterialTheme.colorScheme.onSurfaceVariant,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            if (!nested && item.taskId != null)
                                Icon(
                                    CairnIcons.Retry,
                                    "Mission",
                                    Modifier.padding(start = if (item.finished) 0.dp else 4.dp)
                                        .size(12.dp),
                                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            // The avatar already names the agent: the detail line gives the
                            // project.
                            if (!nested)
                                Text(
                                    (if (item.finished || item.taskId != null) "" else " · ") +
                                        (item.project?.ifBlank { null } ?: item.agent),
                                    Modifier.weight(1f)
                                        .padding(start = if (item.taskId != null) 4.dp else 0.dp),
                                    style = MaterialTheme.typography.labelMedium,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                        }
                    if (
                        item.kind == FeedKind.SET_ASIDE_TASK ||
                            item.kind == FeedKind.SNOOZED_TASK ||
                            item.kind == FeedKind.WAITING_CHAT ||
                            item.kind == FeedKind.WAITING_TASK
                    )
                        Text(
                            item.subtitle,
                            Modifier.padding(start = inset),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    if (
                        item.kind == FeedKind.WAITING_CHAT ||
                            item.kind == FeedKind.WAITING_TASK ||
                            item.kind == FeedKind.SNOOZED_TASK
                    )
                        Text(
                            item.wakeAt?.let { waitLeft(it, item.condition, item.resolved, now) }
                                ?: "Selon disponibilité",
                            Modifier.padding(start = inset).testTag("fil-wait-time"),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.primary,
                        )
                    if (!nested)
                        item.requester?.let {
                            Text(
                                "↳ Délégué par ${it.title}",
                                Modifier.padding(start = inset).testTag("fil-requester"),
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                }
                trailing()
            }
        }
    }
}
