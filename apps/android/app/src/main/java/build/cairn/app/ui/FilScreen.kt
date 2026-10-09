package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.coroutines.delay

internal enum class FeedKind {
    QUESTION,
    FAILED_CHAT,
    RECONNECT,
    FAILED_TASK,
    REVIEW_TASK,
    RUNNING_CHAT,
    RUNNING_TASK,
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
)

internal data class Feed(
    val forYou: List<FeedItem>,
    val running: List<FeedItem>,
    val recent: List<FeedItem>,
) {
    val empty
        get() = forYou.isEmpty() && running.isEmpty() && recent.isEmpty()
}

private fun context(agent: String, project: String?) =
    listOfNotNull(agent.ifBlank { null }, project?.ifBlank { null }).joinToString(" · ")

/**
 * One home for conversations and missions, grouped by what they ask of the user: things that need
 * an answer or a decision, work in progress, then everything else by recency.
 */
internal fun buildFeed(chats: List<Chat>, activity: List<Run>, tasks: List<Task>): Feed {
    val forYou = mutableListOf<FeedItem>()
    val running = mutableListOf<FeedItem>()
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
                                if (chat.status == "queued")
                                    "En attente · ${context(agent, chat.projectName)}"
                                else context(agent, chat.projectName),
                            )
                            .copy(working = chat.status == "running")
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
                                if (run.status == "queued") "Mission en attente · $agent"
                                else "Mission · $agent",
                            )
                            .copy(working = run.status == "running")
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
            }
        }
    return Feed(forYou, running.sortedBy { it.startedAt ?: Long.MAX_VALUE }, recent)
}

internal fun feedSummary(feed: Feed): String {
    val parts = mutableListOf<String>()
    if (feed.running.isNotEmpty())
        parts +=
            if (feed.running.size == 1) "1 agent au travail"
            else "${feed.running.size} agents au travail"
    if (feed.forYou.isNotEmpty())
        parts +=
            if (feed.forYou.size == 1) "1 élément pour vous"
            else "${feed.forYou.size} éléments pour vous"
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
) {
    val live = rememberLive(vm, state, "/chats/stream")
    var activity by remember { mutableStateOf<List<Run>>(emptyList()) }
    Poll("fil-activity", 5000) {
        try {
            activity = vm.api.get("/tasks/activity")
        } catch (e: Exception) {
            vm.report(e)
        }
    }
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(30_000)
            now = System.currentTimeMillis()
        }
    }
    val trash = rememberConversationTrash(vm)
    val chats = live.state?.chats?.filter { it.id !in trash.hidden }
    val feed =
        remember(chats, activity, state.tasks) { buildFeed(chats.orEmpty(), activity, state.tasks) }
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
            Column(
                Modifier.widthIn(max = 760.dp).padding(horizontal = 20.dp).padding(top = 12.dp)
            ) {
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    Wordmark()
                    Spacer(Modifier.weight(1f))
                    RoundAction("Rechercher", CairnIcons.Search, onClick = search)
                }
                Spacer(Modifier.height(16.dp))
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
        section("Pour vous", feed.forYou) { item ->
            FeedRow(
                item,
                { open(item) },
                onDelete = item.chatId?.let { id -> { trash.trash(id) } },
            ) {
                ForYouAction(item, now, !state.busy, { open(item) }, { retry(item) })
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
    row: @Composable (FeedItem) -> Unit,
) {
    if (items.isEmpty()) return
    item(key = "section:$label") {
        Eyebrow(
            label,
            Modifier.padding(start = 20.dp, top = 24.dp, bottom = 4.dp).semantics { heading() },
        )
    }
    items(items, key = { it.key }) { Box(Modifier.animateItem()) { row(it) } }
}

/** The one action a "for you" row offers: answer, reconnect, retry, or just its time. */
@Composable
private fun ForYouAction(
    item: FeedItem,
    now: Long,
    enabled: Boolean,
    open: () -> Unit,
    retry: () -> Unit,
) {
    if (item.kind == FeedKind.QUESTION) SignalButton("Répondre", height = 36.dp, onClick = open)
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
internal fun Wordmark() {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        androidx.compose.foundation.Image(
            painter = androidx.compose.ui.res.painterResource(build.cairn.app.R.drawable.ic_cairn),
            contentDescription = null,
            modifier =
                Modifier.size(40.dp)
                    .background(
                        androidx.compose.ui.graphics.Color(0xFF14365A),
                        androidx.compose.foundation.shape.RoundedCornerShape(10.dp),
                    )
                    .border(
                        1.dp,
                        Color(0xFF346AA3),
                        androidx.compose.foundation.shape.RoundedCornerShape(10.dp),
                    ),
        )
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
            FeedKind.CHAT -> null
        }
    SwipeToTrashRow(enabled = onDelete != null, onTrash = { onDelete?.invoke() }) { swipe ->
        Surface(onClick = onClick, color = Color.Transparent, modifier = swipe.fillMaxWidth()) {
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
                            item.subtitle,
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
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
