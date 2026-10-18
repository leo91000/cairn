package build.cairn.app.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.colorResource
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.R
import build.cairn.app.data.*
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
    CHAT,
}

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
) {
    val unread
        get() = kind == FeedKind.UNREAD_CHAT || kind == FeedKind.UNREAD_TASK

    /** What reading this item marks, with the end of the work it reads. */
    val read
        get() =
            if (kind == FeedKind.UNREAD_TASK) "task:$taskId" to stamp else "chat:$chatId" to stamp
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
    val unread
        get() = forYou.filter { it.unread }
}

private fun context(agent: String, project: String?) =
    listOfNotNull(agent.ifBlank { null }, project?.ifBlank { null }).joinToString(" · ")

/**
 * One home for conversations and missions, grouped by what they ask of the user: things that need
 * an answer or decision, finished work not read yet, work in progress, waiting conversations, then
 * everything else by recency. [read] holds local marks until the server reflects them.
 */
internal fun buildFeed(
    chats: List<Chat>,
    activity: List<Run>,
    tasks: List<Task>,
    read: Map<String, Long> = emptyMap(),
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
                )
            val wait = chatWaitNotice(chat.run)
            when {
                chat.pendingQuestions > 0 ->
                    forYou +=
                        item(
                            FeedKind.QUESTION,
                            if (chat.pendingQuestions == 1) "Une question vous attend"
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
                                // Recorded by GitHub polling; another run's result is only computed
                                // at resumption.
                                resolved = chat.wakeup.observation != null,
                            )
                chat.unread && !chat.paused && unreadNow("chat:${chat.id}", chat.finishedAt) ->
                    toRead +=
                        item(FeedKind.UNREAD_CHAT, context(agent, chat.projectName))
                            .copy(stamp = chat.finishedAt ?: chat.updatedAt, preview = chat.preview)
                else ->
                    recent +=
                        item(
                            FeedKind.CHAT,
                            if (chat.paused) "En pause · ${context(agent, chat.projectName)}"
                            else context(agent, chat.projectName),
                        )
            }
        }
    val names = tasks.associateBy { it.id }
    activity
        .filter { it.trigger != "chat" && it.taskId.isNotBlank() }
        .sortedByDescending { it.finishedAt ?: it.startedAt ?: it.createdAt }
        .forEach { run ->
            val task = names[run.taskId]
            if (task?.archived == true) return@forEach
            val agent = run.snapshot.agent.name.ifBlank { "Agent" }
            val agentKey = run.snapshot.agent.id.ifBlank { task?.agentId.orEmpty() }
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
                )
            when {
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
                    run.outcome.status != "completed" ->
                    forYou += item(FeedKind.REVIEW_TASK, outcomeLabel(run.outcome))
                run.status == "succeeded" &&
                    run.unread &&
                    unreadNow("task:${run.taskId}", run.finishedAt) ->
                    toRead +=
                        item(FeedKind.UNREAD_TASK, "Mission · $agent").copy(preview = run.preview)
            }
        }
    return Feed(
        forYou + toRead.sortedByDescending { it.stamp },
        running.sortedBy { it.startedAt ?: Long.MAX_VALUE },
        recent,
        // A wait whose result arrived resumes first, before any deadline.
        waiting.sortedWith(compareBy({ !it.resolved }, { it.wakeAt })),
    )
}

/** The Fil in reading order, one key per item: what a swipe between pages moves through. */
internal fun Feed.keys() = (forYou + running + waiting + recent).map { it.key }

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
            activity = vm.api.get("/tasks/activity")
        } catch (e: Exception) {
            vm.report(e)
        }
    }
    return activity
}

internal fun feedSummary(feed: Feed): String {
    val parts = mutableListOf<String>()
    if (feed.running.isNotEmpty())
        parts +=
            if (feed.running.size == 1) "1 agent au travail"
            else "${feed.running.size} agents au travail"
    val need = feed.forYou.size - feed.unread.size
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
    val feed = buildFeed(chats.orEmpty(), activity, state.tasks, vm.localReads)
    fun retry(item: FeedItem) {
        val task = item.taskId ?: return
        vm.perform { openRun(api.send<Run>("POST", "/tasks/${segment(task)}/run").id) }
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
                Text(greeting(), style = MaterialTheme.typography.headlineLarge)
                Text(
                    if (chats == null) "Chargement…" else feedSummary(feed),
                    Modifier.padding(top = 4.dp),
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
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
        val unread = feed.unread
        section(
            "Pour vous",
            feed.forYou,
            action =
                if (unread.isEmpty()) null
                else {
                    {
                        SignalButton(
                            "Tout lire",
                            Modifier.testTag("read-all"),
                            icon = CairnIcons.Check,
                            container = Color.Transparent,
                            content = MaterialTheme.colorScheme.onSurfaceVariant,
                            height = 32.dp,
                        ) {
                            vm.markRead(unread.map { it.read })
                        }
                    }
                },
        ) { item ->
            FeedRow(
                item,
                { open(item) },
                onDelete = item.chatId?.let { id -> { trash.trash(id) } },
            ) {
                ForYouAction(
                    item,
                    now,
                    !state.busy,
                    { open(item) },
                    { retry(item) },
                    { vm.markRead(listOf(item.read)) },
                )
            }
        }
        section("En cours", feed.running) { item ->
            FeedRow(
                item,
                { open(item) },
                onDelete = item.chatId?.let { id -> { trash.trash(id) } },
            ) {
                Text(
                    elapsed(item.startedAt, now),
                    style = MaterialTheme.typography.labelLarge,
                    fontWeight = FontWeight.SemiBold,
                )
            }
        }
        section("Reprise prévue", feed.waiting) { item ->
            FeedRow(
                item,
                { open(item) },
                onDelete = item.chatId?.let { id -> { trash.trash(id) } },
            ) {
                Text(
                    waitLeft(item.wakeAt ?: now, item.condition, item.resolved, now),
                    style = MaterialTheme.typography.labelLarge,
                    fontWeight = FontWeight.SemiBold,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
        }
        section("Récents", feed.recent) { item ->
            FeedRow(
                item,
                { open(item) },
                onDelete = item.chatId?.let { id -> { trash.trash(id) } },
            ) {
                Stamp(item.stamp, now)
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

/** The one action a "for you" row offers: answer, reconnect, retry, mark read, or its time. */
@Composable
private fun ForYouAction(
    item: FeedItem,
    now: Long,
    enabled: Boolean,
    open: () -> Unit,
    retry: () -> Unit,
    markRead: () -> Unit,
) {
    if (item.unread)
        RoundAction(
            "Marquer ${item.title} comme lu",
            CairnIcons.Check,
            size = 38.dp,
            onClick = markRead,
        )
    else if (item.kind == FeedKind.QUESTION)
        SignalButton("Répondre", height = 36.dp, onClick = open)
    else if (item.kind == FeedKind.RECONNECT)
        SignalButton("Connexions", height = 36.dp, onClick = open)
    else if (item.kind == FeedKind.FAILED_TASK)
        RoundAction(
            "Relancer ${item.title}",
            CairnIcons.Retry,
            size = 38.dp,
            enabled = enabled,
            onClick = retry,
        )
    else Stamp(item.stamp, now)
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

@Composable
private fun FeedRow(
    item: FeedItem,
    onClick: () -> Unit,
    onDelete: (() -> Unit)? = null,
    trailing: @Composable () -> Unit,
) {
    val badge =
        when (item.kind) {
            FeedKind.RUNNING_CHAT,
            FeedKind.RUNNING_TASK -> AvatarBadge.LIVE
            FeedKind.QUESTION,
            FeedKind.FAILED_CHAT,
            FeedKind.FAILED_TASK,
            FeedKind.REVIEW_TASK,
            FeedKind.RECONNECT -> AvatarBadge.ATTENTION
            FeedKind.UNREAD_CHAT,
            FeedKind.UNREAD_TASK -> AvatarBadge.DONE
            FeedKind.WAITING_CHAT -> AvatarBadge.WAITING
            FeedKind.CHAT -> null
        }
    SwipeToTrashRow(enabled = onDelete != null, onTrash = { onDelete?.invoke() }) { swipe ->
        // Unread work keeps a faint wash of the accent until it is read.
        Surface(
            onClick = onClick,
            color =
                if (item.unread) MaterialTheme.colorScheme.primary.copy(alpha = 0.06f)
                else Color.Transparent,
            modifier =
                swipe
                    .fillMaxWidth()
                    .then(if (item.unread) Modifier.testTag("unread") else Modifier),
        ) {
            Row(
                Modifier.widthIn(max = 760.dp)
                    .heightIn(min = 72.dp)
                    .padding(horizontal = 20.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (item.working) WorkingAvatar(item.agent, item.agentKey, 44.dp)
                else AgentAvatar(item.agent, item.agentKey, 44.dp, badge)
                Spacer(Modifier.width(14.dp))
                Column(Modifier.weight(1f)) {
                    if (item.unread) UnreadLabel(item)
                    Text(
                        item.title,
                        style = MaterialTheme.typography.titleMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Row(
                        Modifier.padding(top = 2.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        if (item.working) WorkingLabel("En cours", Modifier.padding(end = 6.dp))
                        Text(
                            item.preview ?: item.subtitle,
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = if (item.preview != null) 2 else 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
                Spacer(Modifier.width(10.dp))
                trailing()
            }
        }
    }
}

/** "Terminé" with the unread dot, above the title of finished work not read yet. */
@Composable
private fun UnreadLabel(item: FeedItem) {
    Row(Modifier.padding(bottom = 2.dp), verticalAlignment = Alignment.CenterVertically) {
        Box(
            Modifier.size(8.dp)
                .background(
                    MaterialTheme.colorScheme.primary,
                    androidx.compose.foundation.shape.CircleShape,
                )
                .semantics { contentDescription = "Non lu" }
        )
        Spacer(Modifier.width(6.dp))
        Text(
            if (item.kind == FeedKind.UNREAD_TASK) "Mission terminée · ${shortStamp(item.stamp)}"
            else "Terminé · ${shortStamp(item.stamp)}",
            style = MaterialTheme.typography.labelMedium,
            fontWeight = FontWeight.SemiBold,
            color = MaterialTheme.colorScheme.primary,
        )
    }
}
