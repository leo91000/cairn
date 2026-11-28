@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.ExperimentalFoundationApi::class,
)

package build.cairn.app.ui

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.colorResource
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import build.cairn.app.R
import build.cairn.app.data.*
import kotlinx.coroutines.delay

internal enum class FeedKind {
    QUESTION,
    FAILED_CHAT,
    RECONNECT,
    REVIEW,
    UNREAD_CHAT,
    RUNNING_CHAT,
    WAITING_CHAT,
    SNOOZED,
    CHAT,
}

internal data class FeedRequester(val title: String, val chatId: String)

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
    val requester: FeedRequester? = null,
    val originRule: build.cairn.contracts.ChatRuleOrigin? = null,
    /** The coding agent of its latest turn. */
    val harness: String? = null,
    val statusLabel: String = subtitle,
    val project: String? = null,
    val unread: Boolean = kind == FeedKind.UNREAD_CHAT,
    val readAt: Long = stamp,
    val snoozeAt: Long = readAt,
    /** It finished as expected: its row shows no status label, only a quiet mark. */
    val finished: Boolean = false,
) {
    /** What reading this item marks, with the end of the work it reads. */
    val read
        get() = key to readAt
}

internal data class Feed(
    val forYou: List<FeedItem>,
    val running: List<FeedItem>,
    val recent: List<FeedItem>,
    /** Conversations that resume on their own, soonest first. */
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
                    FeedKind.REVIEW,
                    FeedKind.RUNNING_CHAT,
                    FeedKind.WAITING_CHAT,
                    FeedKind.SNOOZED,
                )
        }
}

private fun context(agent: String, project: String?) =
    listOfNotNull(agent.ifBlank { null }, project?.ifBlank { null }).joinToString(" · ")

/** 9:00 tomorrow, local time: when a snoozed conversation asks again. */
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

internal fun buildFeed(
    chats: List<Chat>,
    read: Map<String, Long> = emptyMap(),
    now: Long = System.currentTimeMillis(),
    usable: List<String>? = null,
): Feed {
    fun unreadNow(key: String, finishedAt: Long?) =
        (read[key] ?: Long.MIN_VALUE) < (finishedAt ?: 0)
    val forYou = mutableListOf<FeedItem>()
    val toRead = mutableListOf<FeedItem>()
    val running = mutableListOf<FeedItem>()
    val waiting = mutableListOf<FeedItem>()
    val recent = mutableListOf<FeedItem>()
    chats
        .filter { it.lifecycle == "active" }
        .sortedByDescending { it.updatedAt }
        .forEach { chat ->
            val agent = chat.agentName.ifBlank { "Agent" }
            val activityAt = chat.readAt ?: chat.finishedAt ?: chat.updatedAt
            val until = chat.snoozedUntil
            val reminderDue = until != null && chat.unread && activityAt >= until
            val snoozed = until != null && until > now && !reminderDue
            val unread =
                !snoozed &&
                    chat.unread &&
                    unreadNow("chat:${chat.id}", activityAt) &&
                    (reminderDue ||
                        chat.requestedBy == null ||
                        chat.pendingQuestions > 0 ||
                        chat.outcome?.status == "blocked")
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
                    // A delegated conversation names the conversation that delegated it.
                    requester =
                        chat.requester?.let {
                            FeedRequester(
                                it.title.ifBlank { "Nouvelle conversation" },
                                it.chatId,
                            )
                        },
                    harness = chat.provider,
                    originRule = chat.originRule,
                    project = chat.projectName,
                    statusLabel =
                        when (kind) {
                            FeedKind.QUESTION -> "Répondre"
                            FeedKind.RECONNECT -> "Connexion requise"
                            FeedKind.REVIEW -> "Bloquée"
                            FeedKind.FAILED_CHAT -> "Échec"
                            FeedKind.SNOOZED -> "Reportée"
                            FeedKind.RUNNING_CHAT ->
                                if (chat.exclusiveSuspended) "Suspendue"
                                else if (chat.status == "queued") "En attente" else "En cours"
                            FeedKind.WAITING_CHAT -> "Reprise prévue"
                            else -> if (chat.paused) "En pause" else statusLabel(chat.status)
                        },
                    readAt = activityAt,
                    snoozeAt = maxOf(activityAt, chat.lastActivityAt ?: 0),
                    unread = unread,
                )
            val wait = chatWaitNotice(chat.run)
            when {
                snoozed -> waiting += item(FeedKind.SNOOZED, "Reportée").copy(wakeAt = until)
                chat.pendingQuestions > 0 && !canDirectChat(usable, chat, chats) ->
                    waiting +=
                        item(
                            FeedKind.WAITING_CHAT,
                            "En attente d’une personne ayant accès à l’agent",
                        )
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
                chat.status in setOf("failed", "interrupted") ||
                    (chat.paused && chat.error != null) ->
                    forYou +=
                        item(
                            FeedKind.FAILED_CHAT,
                            chat.error?.lineSequence()?.firstOrNull()?.ifBlank { null }
                                ?: statusLabel(chat.status),
                        )
                chat.status == "succeeded" && chat.outcome?.status == "blocked" -> {
                    val outcome = chat.outcome
                    val label = "Bloquée · ${outcome.reason}"
                    if (unread) forYou += item(FeedKind.REVIEW, label)
                    else
                        recent +=
                            item(FeedKind.CHAT, label).copy(unread = false, statusLabel = "Bloquée")
                }
                reminderDue && unread ->
                    toRead += item(FeedKind.UNREAD_CHAT, "Report terminé").copy(stamp = activityAt)
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
                unread && !chat.paused ->
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
    return Feed(
        forYou + toRead.sortedByDescending { it.stamp },
        running.sortedBy { it.startedAt ?: Long.MAX_VALUE },
        recent.sortedByDescending { it.stamp },
        // A wait whose result arrived resumes first, before any deadline.
        waiting.sortedWith(compareBy({ !it.resolved }, { it.wakeAt ?: Long.MAX_VALUE })),
    )
}

/**
 * Rows in reading order, each delegated conversation right below the work that started it when both
 * are listed. One that started without its requester in the list stays in place and names it
 * instead. The second value tells whether the row is nested.
 */
internal fun threadFeed(items: List<FeedItem>): List<Pair<FeedItem, Boolean>> {
    val byChat = items.filter { it.chatId != null }.associateBy { it.chatId }
    val childrenOf = mutableMapOf<String, MutableList<FeedItem>>()
    val roots = mutableListOf<FeedItem>()
    items.forEach { item ->
        val parent = item.requester?.let { byChat[it.chatId] }
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
internal fun Feed.keys() = threadFeed(all).map { (item, _) -> item.key }

/** The shell and its pages share one conversation stream . */
internal data class FilSnapshot(val conversations: LiveSnapshot)

internal val LocalFilSnapshot = compositionLocalOf<FilSnapshot?> { null }

@Composable
internal fun rememberFilConversations(vm: CairnViewModel, state: Workspace): LiveSnapshot =
    LocalFilSnapshot.current?.conversations ?: rememberLive(vm, state, "/chats/stream")

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
    openConnections: () -> Unit,
    search: () -> Unit,
    openAccount: () -> Unit = {},
    openSettings: () -> Unit = openAccount,
) {
    val live = rememberFilConversations(vm, state)
    var connection by rememberSaveable { mutableStateOf(false) }
    if (connection)
        ConnectionSheet(vm, state, close = { connection = false }, openSettings = openSettings)
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(30_000)
            now = System.currentTimeMillis()
        }
    }
    val trash = rememberConversationTrash(vm)
    val chats = live.state?.chats?.filter { it.id !in trash.hidden }
    val feed = buildFeed(chats.orEmpty(), vm.localReads, now, state.usableAgents)
    var selectedView by rememberSaveable { mutableIntStateOf(0) }
    // A finished result previewed by a long press, without opening or reading it.
    var peeked by remember { mutableStateOf<FeedItem?>(null) }
    val views = listOf(feed.unread, feed.ongoing, feed.all)
    val visible = views[selectedView]
    fun open(item: FeedItem) {
        when {
            item.kind == FeedKind.RECONNECT -> openConnections()
            item.chatId != null -> openChat(item.chatId)
        }
    }
    var snoozing by remember { mutableStateOf<FeedItem?>(null) }
    snoozing?.let { item ->
        SnoozeConversationDialog(item.title, dismiss = { snoozing = null }) { until ->
            snoozing = null
            item.chatId?.let { vm.snooze(it, item.snoozeAt, until) }
        }
    }
    peeked?.let { item ->
        PeekSheet(item, now, close = { peeked = null }) {
            peeked = null
            open(item)
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
                    showOngoing = { selectedView = 1 },
                    readAll = { vm.markRead(feed.unread.map { it.read }) },
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
                        "Appuyez sur + pour démarrer une conversation.",
                    )
                }
            }
        if (chats != null && visible.isEmpty() && !feed.empty)
            item(key = "view-empty") {
                Text(
                    if (selectedView == 0) "Tout est lu." else "Aucune tâche en cours.",
                    Modifier.padding(20.dp),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        items(threadFeed(visible), key = { it.first.key }) { (item, nested) ->
            FeedRow(
                item,
                { open(item) },
                onDelete =
                    item.chatId
                        ?.takeIf { canUseAgent(state.usableAgents, item.agentKey) }
                        ?.let { id -> { trash.trash(id) } },
                now = now,
                nested = nested,
                onPeek = item.preview?.ifBlank { null }?.let { { peeked = item } },
            ) {
                FilRowActions(
                    item,
                    { vm.markRead(listOf(item.read), clearSnooze = item.kind == FeedKind.SNOOZED) },
                    snooze = { snoozing = item },
                    restore = { item.chatId?.let(vm::restoreSnooze) },
                )
            }
        }
    }
}

@Composable
internal fun PeekSheet(item: FeedItem, now: Long, close: () -> Unit, open: () -> Unit) {
    ModalBottomSheet(
        onDismissRequest = close,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        modifier = Modifier.testTag("fil-peek"),
    ) {
        Column(
            Modifier.verticalScroll(rememberScrollState())
                .padding(start = 24.dp, end = 24.dp, bottom = 24.dp)
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                AgentAvatar(item.agent, item.agentKey.ifBlank { item.agent }, 24.dp)
                Spacer(Modifier.width(10.dp))
                Text(
                    listOfNotNull(item.agent, item.project?.ifBlank { null }).joinToString(" · "),
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Stamp(item.stamp, now)
            }
            Text(
                item.title,
                Modifier.padding(top = 12.dp),
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                item.preview.orEmpty(),
                Modifier.padding(top = 8.dp),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 12,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                "Reste non lu jusqu’à l’ouverture.",
                Modifier.padding(top = 12.dp),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            androidx.compose.material3.Button(
                onClick = open,
                modifier = Modifier.fillMaxWidth().padding(top = 16.dp),
            ) {
                Text("Ouvrir")
            }
        }
    }
}

/** The Fil's title, the agents at work, and its read-all action. */
@Composable
private fun FilHeader(
    working: List<FeedItem>,
    unread: Boolean,
    showOngoing: () -> Unit,
    readAll: () -> Unit,
) {
    // Keeps the row's height when its only action, read-all, comes and goes.
    Row(Modifier.heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) {
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
        if (unread) {
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

@Composable
internal fun FilRowActions(
    item: FeedItem,
    markRead: () -> Unit,
    snooze: () -> Unit,
    restore: () -> Unit,
) {
    if (item.unread && item.readAt > 0)
        RoundAction(
            "Marquer comme lu : ${item.title}",
            CairnIcons.Check,
            size = 36.dp,
            onClick = markRead,
        )
    var menu by remember { mutableStateOf(false) }
    Box {
        RoundAction("Autres actions pour ${item.title}", CairnIcons.More, size = 36.dp) {
            menu = true
        }
        DropdownMenu(menu, { menu = false }) {
            if ((item.unread || item.kind == FeedKind.SNOOZED) && item.readAt > 0)
                DropdownMenuItem(
                    text = { Text("Marquer comme lu") },
                    onClick = {
                        menu = false
                        markRead()
                    },
                )
            DropdownMenuItem(
                text = {
                    Text(if (item.kind == FeedKind.SNOOZED) "Rétablir" else "Reporter jusqu’à…")
                },
                onClick = {
                    menu = false
                    if (item.kind == FeedKind.SNOOZED) restore() else snooze()
                },
            )
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
internal fun FilAvatar(item: FeedItem, size: androidx.compose.ui.unit.Dp) {
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
            item.kind == FeedKind.QUESTION -> CairnIcons.CircleAlert to signal.answer
            item.kind == FeedKind.RECONNECT -> CairnIcons.CircleAlert to signal.warning
            item.kind == FeedKind.FAILED_CHAT -> CairnIcons.CircleX to signal.attention
            item.kind == FeedKind.REVIEW -> CairnIcons.CirclePause to signal.warning
            item.kind == FeedKind.RUNNING_CHAT ->
                CairnIcons.CircleProgress to MaterialTheme.colorScheme.primary
            item.kind == FeedKind.WAITING_CHAT || item.kind == FeedKind.SNOOZED ->
                CairnIcons.Clock to MaterialTheme.colorScheme.primary
            item.finished ->
                CairnIcons.CircleCheck to
                    MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.7f)
            item.kind == FeedKind.CHAT && item.statusLabel == "En pause" ->
                CairnIcons.CirclePause to MaterialTheme.colorScheme.onSurfaceVariant
            else -> CairnIcons.Circle to MaterialTheme.colorScheme.onSurfaceVariant
        }
    val rotation =
        if (item.working)
            rememberInfiniteTransition(label = "fil-working")
                .animateFloat(
                    0f,
                    360f,
                    infiniteRepeatable(tween(1000, easing = LinearEasing)),
                    label = "rotation",
                )
        else null
    Icon(
        icon,
        contentDescription = item.statusLabel,
        modifier =
            Modifier.size(14.dp).testTag("fil-mark").graphicsLayer {
                rotationZ = rotation?.value ?: 0f
            },
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
    /** Previews a finished result without opening it. */
    onPeek: (() -> Unit)? = null,
    trailing: @Composable () -> Unit,
) {
    val thread = MaterialTheme.colorScheme.outlineVariant
    val haptics = LocalHapticFeedback.current
    // Where the text below the title starts: after the unread dot, the avatar and its gap.
    val inset = if (nested) 42.dp else 50.dp
    SwipeToTrashRow(enabled = onDelete != null, onTrash = { onDelete?.invoke() }) { swipe ->
        Surface(
            color = Color.Transparent,
            modifier =
                swipe
                    .fillMaxWidth()
                    .semantics { if (item.finished) stateDescription = item.statusLabel }
                    .combinedClickable(
                        onClick = onClick,
                        onLongClickLabel = onPeek?.let { "Aperçu" },
                        onLongClick =
                            onPeek?.let {
                                {
                                    haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                                    it()
                                }
                            },
                    )
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
                    val needsAnswer = item.kind == FeedKind.QUESTION
                    val failed = item.kind == FeedKind.FAILED_CHAT
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
                                        else if (item.kind == FeedKind.REVIEW) signal.warning
                                        else MaterialTheme.colorScheme.onSurfaceVariant,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            // The avatar already names the agent: the detail line gives the
                            // project.
                            if (!nested)
                                Text(
                                    (if (item.finished) "" else " · ") +
                                        (item.project?.ifBlank { null } ?: item.agent),
                                    Modifier.weight(1f),
                                    style = MaterialTheme.typography.labelMedium,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                        }
                    if (item.kind == FeedKind.SNOOZED || item.kind == FeedKind.WAITING_CHAT)
                        Text(
                            item.subtitle,
                            Modifier.padding(start = inset),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    if (item.kind == FeedKind.WAITING_CHAT || item.kind == FeedKind.SNOOZED)
                        Text(
                            item.wakeAt?.let { waitLeft(it, item.condition, item.resolved, now) }
                                ?: "Selon disponibilité",
                            Modifier.padding(start = inset).testTag("fil-wait-time"),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.primary,
                        )
                    item.originRule?.let { origin ->
                        val type = if (origin.schedule) "Programmation" else "Règle"
                        Text(
                            "$type · ${origin.ruleName}",
                            Modifier.padding(start = inset),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
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
