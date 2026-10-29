@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
    androidx.compose.foundation.ExperimentalFoundationApi::class,
)

package build.cairn.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.content.contentReceiver
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.input.InputTransformation
import androidx.compose.foundation.text.input.TextFieldLineLimits
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.clearText
import androidx.compose.foundation.text.input.maxLength
import androidx.compose.foundation.text.input.setTextAndPlaceCursorAtEnd
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.SecureFlagPolicy
import build.cairn.app.data.*
import build.cairn.contracts.ResumeRunInput
import build.cairn.contracts.resumeRun
import java.io.File
import java.util.UUID
import kotlinx.serialization.json.*

@Composable
fun ChatsScreen(vm: CairnViewModel, state: Workspace, open: (String) -> Unit, create: () -> Unit) {
    val live = rememberLive(vm, state, "/chats/stream")
    ConversationList(
        live.state?.chats.orEmpty(),
        null,
        live.state == null,
        live.error,
        open,
        create,
        vm,
    )
}

@Composable
internal fun ConversationList(
    activeChats: List<Chat>,
    selected: String?,
    loading: Boolean,
    error: String?,
    open: (String) -> Unit,
    create: () -> Unit,
    vm: CairnViewModel,
) {
    var view by rememberSaveable { mutableStateOf("active") }
    var menu by remember { mutableStateOf(false) }
    val usableAgents = vm.state.collectAsState().value.usableAgents
    var other by remember { mutableStateOf<List<Chat>>(emptyList()) }
    var revision by remember { mutableIntStateOf(0) }
    var listError by remember { mutableStateOf<String?>(null) }
    var fetching by remember { mutableStateOf(false) }
    LaunchedEffect(view, revision) {
        other = emptyList()
        if (view != "active") {
            fetching = true
            try {
                other = vm.api.get("/chats?view=$view")
            } catch (e: Exception) {
                listError = e.message
            } finally {
                fetching = false
            }
        }
    }
    // The list can sit in a bottom sheet, above the app snackbar: it hosts its own undo.
    val snackbar = remember { SnackbarHostState() }
    val trash = rememberConversationTrash(vm, snackbar) { revision++ }
    val chats =
        when (view) {
            "active" -> activeChats.filter { it.id !in trash.hidden }
            "trash" -> other
            else -> other.filter { it.id !in trash.hidden }
        }
    var query by rememberSaveable { mutableStateOf("") }
    val today =
        java.time.LocalDate.now()
            .atStartOfDay(java.time.ZoneId.systemDefault())
            .toInstant()
            .toEpochMilli()
    val yesterday =
        java.time.LocalDate.now()
            .minusDays(1)
            .atStartOfDay(java.time.ZoneId.systemDefault())
            .toInstant()
            .toEpochMilli()
    val filtered =
        chats
            .filter {
                "${it.title} ${it.agentName} ${it.projectName.orEmpty()}"
                    .contains(query.trim(), true)
            }
            .sortedByDescending { it.updatedAt }
    val groups =
        listOf(
            "Aujourd’hui" to filtered.filter { it.updatedAt >= today },
            "Hier" to filtered.filter { it.updatedAt in yesterday until today },
            "Plus tôt" to filtered.filter { it.updatedAt < yesterday },
        )
    Box(Modifier.fillMaxSize()) {
        Column(Modifier.fillMaxSize().padding(horizontal = 16.dp)) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "Conversations",
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.headlineSmall,
                )
                Box {
                    ActionIcon("Afficher les conversations", CairnIcons.More) { menu = true }
                    DropdownMenu(menu, { menu = false }) {
                        listOf("active" to "Actives", "trash" to "Corbeille").forEach { (key, label)
                            ->
                            DropdownMenuItem(
                                text = { Text(label) },
                                onClick = {
                                    view = key
                                    menu = false
                                },
                            )
                        }
                    }
                }
                ActionIcon("Nouvelle conversation", CairnIcons.Plus, onClick = create)
            }
            if (view != "active") Text("Corbeille", style = MaterialTheme.typography.labelMedium)
            SearchField("Rechercher une conversation", query) { query = it }
            if (loading || fetching) LinearProgressIndicator(Modifier.fillMaxWidth())
            (listError ?: error)?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            LazyColumn(
                Modifier.weight(1f),
                contentPadding = PaddingValues(vertical = 12.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                if (!loading && filtered.isEmpty())
                    item {
                        Empty(
                            if (query.isBlank()) "Aucune conversation" else "Aucun résultat",
                            if (query.isBlank()) "Confiez une mission à votre agent pour commencer."
                            else "Essayez un autre titre, agent ou projet.",
                        )
                    }
                groups
                    .filter { it.second.isNotEmpty() }
                    .forEach { (label, items) ->
                        item(key = label) {
                            Text(
                                label,
                                Modifier.padding(top = 12.dp, bottom = 8.dp, start = 8.dp),
                                style = MaterialTheme.typography.labelMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        items(items, key = { it.id }) { chat ->
                            Box(Modifier.animateItem()) {
                                key(view) {
                                    SwipeToTrashRow(
                                        // Deleting needs access to the conversation's agent.
                                        enabled =
                                            view != "trash" &&
                                                canUseAgent(usableAgents, chat.agentId),
                                        onTrash = { trash.trash(chat.id) },
                                    ) { swipe ->
                                        Surface(
                                            modifier = swipe,
                                            onClick = { open(chat.id) },
                                            color =
                                                if (chat.id == selected)
                                                    MaterialTheme.colorScheme.primaryContainer
                                                else
                                                    MaterialTheme.colorScheme.surfaceContainerLow
                                                        .copy(alpha = 0f),
                                            shape = RoundedCornerShape(16.dp),
                                        ) {
                                            Row(
                                                Modifier.fillMaxWidth()
                                                    .heightIn(min = 64.dp)
                                                    .padding(horizontal = 8.dp, vertical = 8.dp),
                                                verticalAlignment = Alignment.CenterVertically,
                                            ) {
                                                val working =
                                                    chat.pendingQuestions == 0 &&
                                                        !chat.paused &&
                                                        !chat.exclusiveSuspended &&
                                                        chat.status == "running"
                                                if (working)
                                                    WorkingAvatar(
                                                        chat.agentName.ifBlank { chat.title },
                                                        chat.agentId,
                                                        40.dp,
                                                    )
                                                else
                                                    AgentAvatar(
                                                        chat.agentName.ifBlank { chat.title },
                                                        chat.agentId,
                                                        40.dp,
                                                        when {
                                                            chat.pendingQuestions > 0 ||
                                                                chat.status in
                                                                    setOf(
                                                                        "failed",
                                                                        "interrupted",
                                                                    ) -> AvatarBadge.ATTENTION
                                                            !chat.paused &&
                                                                !chat.exclusiveSuspended &&
                                                                chat.status == "queued" ->
                                                                AvatarBadge.LIVE
                                                            else -> null
                                                        },
                                                    )
                                                Spacer(Modifier.width(12.dp))
                                                Column(Modifier.weight(1f)) {
                                                    Text(
                                                        chat.title,
                                                        style = MaterialTheme.typography.titleSmall,
                                                        maxLines = 2,
                                                        overflow = TextOverflow.Ellipsis,
                                                    )
                                                    if (working)
                                                        WorkingLabel(statusLabel(chat.status))
                                                    else
                                                        Text(
                                                            when {
                                                                chat.pendingQuestions > 0 ->
                                                                    "${chat.pendingQuestions} question(s) en attente"
                                                                chat.paused -> "En pause"
                                                                chat.exclusiveSuspended ->
                                                                    "Suspendue (exclusivité)"
                                                                chat.status in
                                                                    listOf(
                                                                        "running",
                                                                        "queued",
                                                                        "failed",
                                                                        "interrupted",
                                                                    ) -> statusLabel(chat.status)
                                                                chat.wakeup != null &&
                                                                    chat.status == "succeeded" ->
                                                                    waitTarget(chat.wakeup).short
                                                                else ->
                                                                    listOfNotNull(
                                                                            chat.agentName.ifBlank {
                                                                                null
                                                                            },
                                                                            chat.projectName,
                                                                        )
                                                                        .joinToString(" · ")
                                                            },
                                                            style =
                                                                MaterialTheme.typography.bodySmall,
                                                            color =
                                                                if (
                                                                    chat.pendingQuestions > 0 ||
                                                                        chat.status in
                                                                            setOf(
                                                                                "failed",
                                                                                "interrupted",
                                                                            )
                                                                )
                                                                    signal.attention
                                                                else
                                                                    MaterialTheme.colorScheme
                                                                        .onSurfaceVariant,
                                                            maxLines = 1,
                                                            overflow = TextOverflow.Ellipsis,
                                                        )
                                                }
                                                Spacer(Modifier.width(8.dp))
                                                Text(
                                                    shortStamp(chat.updatedAt),
                                                    style = MaterialTheme.typography.labelSmall,
                                                    color =
                                                        MaterialTheme.colorScheme.onSurfaceVariant,
                                                )
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
            }
        }
        SnackbarHost(snackbar, Modifier.align(Alignment.BottomCenter))
    }
}

/**
 * A saved conversation is current when the Fil shows nothing new since it was saved: the same
 * finished turn, no work in progress and no later change, as on the web.
 */
internal fun savedIsCurrent(saved: CachedHistory, chats: List<Chat>?): Boolean {
    val chat = saved.state.chat ?: return true
    val known = chats?.find { it.id == chat.id } ?: return true
    val run = saved.state.run
    return known.runId == run?.id &&
        known.status == (run?.status ?: "idle") &&
        known.status !in listOf("queued", "running") &&
        known.finishedAt == run?.finishedAt &&
        known.updatedAt <= chat.updatedAt
}

@Composable
internal fun ChatPage(
    vm: CairnViewModel,
    state: Workspace,
    id: String?,
    conversations: LiveSnapshot,
    selected: Boolean = true,
    initialAgent: String = MAIN_AGENT_ID,
    initialProject: String = "",
    openChat: (String) -> Unit,
    openRun: (String) -> Unit,
    back: () -> Unit = {},
    create: () -> Unit = back,
    openConnections: () -> Unit = {},
    openSettings: () -> Unit = {},
) {
    val live =
        if (id == null) conversations
        else
            rememberLive(
                vm,
                state,
                "/chats/${segment(id)}/stream",
                streaming = selected,
                current = { savedIsCurrent(it, conversations.state?.chats) },
            )
    val chat = live.state?.chat?.takeIf { it.id == id }
    ReadWhileOpen(
        vm,
        chat
            ?.run
            ?.takeIf { selected && live.synced && !live.catchingUp && it.status == "succeeded" }
            ?.finishedAt
            ?.let { "chat:${chat.id}" to it },
    )
    var createdId by rememberSaveable(id) { mutableStateOf<String?>(null) }
    var agent by rememberSaveable(id) { mutableStateOf(initialAgent) }
    // The member's agents may load after the new conversation opened.
    LaunchedEffect(state.usableAgents, id) {
        if (id == null && !canUseAgent(state.usableAgents, agent))
            agent = defaultAgent(state.usableAgents, state.agents)
    }
    var project by rememberSaveable(id) { mutableStateOf(initialProject) }
    val draftKey = id ?: "new:$initialAgent:$initialProject"
    val savedDraft = remember(draftKey) { vm.chatDrafts[draftKey] ?: ChatDraft() }
    // Keeps the caret and IME composition; `draft` reads its text live.
    val composer =
        rememberSaveable(id, saver = TextFieldState.Saver) {
            TextFieldState(savedDraft.text, TextRange(savedDraft.text.length))
        }
    val draft by remember(composer) { derivedStateOf { composer.text.toString() } }
    var dismissedMention by remember(id) { mutableStateOf<Int?>(null) }
    var provider by rememberSaveable(id) { mutableStateOf(savedDraft.provider) }
    var model by rememberSaveable(id) { mutableStateOf(savedDraft.model) }
    var reasoning by rememberSaveable(id) { mutableStateOf(savedDraft.reasoning) }
    var speed by rememberSaveable(id) { mutableStateOf(savedDraft.speed) }
    var attachments by rememberForm(savedDraft.attachments)
    // Per-file states of the composer: refused before upload, uploading, or failed to upload.
    var rejected by rememberForm(savedDraft.rejected)
    val uploadProgress = remember(id) { mutableStateMapOf<String, Float>() }
    val uploadErrors = remember(id) { mutableStateMapOf<String, String>() }
    var attaching by rememberSaveable(id) { mutableStateOf(false) }
    var editing by rememberSaveable(id) { mutableStateOf(savedDraft.editing) }
    var submissionId by rememberSaveable(id) { mutableStateOf(savedDraft.submissionId) }
    var submissionKey by rememberSaveable(id) { mutableStateOf(savedDraft.submissionKey) }
    var follow by rememberSaveable(id) { mutableStateOf(true) }
    var gallery by rememberSaveable(id) { mutableStateOf(false) }
    var menu by remember { mutableStateOf(false) }
    var fullscreen by rememberSaveable(id) { mutableStateOf(false) }
    var queueExpanded by rememberSaveable(id) { mutableStateOf(false) }
    var newSession by remember(id) { mutableStateOf(false) }
    var compacting by remember(id) { mutableStateOf(false) }
    var cancelledExpanded by rememberSaveable(id) { mutableStateOf(false) }
    BackHandler(selected && fullscreen) { fullscreen = false }
    var choosing by rememberSaveable(id) { mutableStateOf(false) }
    var details by rememberSaveable(id) { mutableStateOf(false) }
    var connection by rememberSaveable(id) { mutableStateOf(false) }
    var outgoing by remember(id) { mutableStateOf<ChatMessage?>(null) }
    fun persistDraft() {
        if (vm.state.value.session.authenticated && !vm.state.value.signingOut)
            vm.chatDrafts[draftKey] =
                ChatDraft(
                    draft,
                    model,
                    reasoning,
                    attachments,
                    editing,
                    submissionId,
                    submissionKey,
                    provider,
                    rejected,
                    speed,
                )
    }
    SideEffect { persistDraft() }
    DisposableEffect(draftKey) { onDispose { persistDraft() } }
    LaunchedEffect(chat?.messages, live.events) {
        outgoing?.let { pending ->
            if (
                chat?.messages.orEmpty().any { it.id == pending.id && it.status == "delivered" } ||
                    live.events.any {
                        it.type == "chat.user" &&
                            it.payload?.get("messageId")?.jsonPrimitive?.contentOrNull == pending.id
                    }
            )
                outgoing = null
        }
    }
    val timelineKeys = remember(id) { TimelineKeys() }
    val quota = remember(chat?.run, live.events) { chatQuota(chat?.run, live.events) }
    // A failed or interrupted turn never resumes its wakeup, even when the record lingers.
    val pendingWakeup = liveWakeup(chat)
    val history =
        remember(
            live.events,
            live.state?.artifacts,
            pendingWakeup?.id,
            pendingWakeup?.observation,
        ) {
            deliveryTimeline(
                timelineKeys.stabilize(
                    live.history,
                    timelineEntries(live.events, chat = true, pendingWakeup = pendingWakeup),
                ),
                live.state?.artifacts.orEmpty(),
            )
        }
    val chats = conversations.state?.chats
    // A member without access to the conversation's agent follows it without directing it.
    val readOnly = chat != null && !canDirectChat(state.usableAgents, chat, chats.orEmpty())
    // A member the owner has not yet given any agent cannot start a conversation.
    val noAgent = id == null && state.usableAgents?.isEmpty() == true
    val timeline =
        remember(history, live.events, chats) {
            quotaTimeline(history, live.events).followingDelegations(chats)
        }
    var asking by remember { mutableStateOf<ChatQuestion?>(null) }
    var stopping by remember { mutableStateOf(false) }
    var removing by remember { mutableStateOf<ChatMessage?>(null) }
    val listState = rememberLazyListState()
    val rendering = remember(id) { MarkdownRendering() }
    val positionReady =
        rememberHistoryPosition(
            vm,
            state,
            if (id == null) "/chats/stream" else "/chats/${segment(id)}/stream",
            live,
            listState,
            !gallery,
            follow,
            rendering,
        ) {
            follow = it
        }
    // Neighbors need a real preview, not an unbounded backwards history crawl.
    val loadOlder = rememberHistoryPaging(live, listState, selected && positionReady && !gallery)
    val active = chat?.run?.active == true
    val running = chat?.run?.status == "running" && quota == null
    val waitingTasks =
        remember(live.events, running) {
            if (running) backgroundWait(live.events) else null
        }
    // A subagent's full activity opens in a sheet. The current request's subagents stay in sight
    // above the composer while one of them works.
    var openedSubagent by rememberSaveable(id) { mutableStateOf<String?>(null) }
    val subagentTeams = remember(timeline) { subagentTeams(timeline) }
    // A delegated task is a conversation of its own: open it rather than a sheet.
    val openSubagent =
        remember(subagentTeams) {
            { agent: String ->
                val delegation = subagentTeams.flatten().find { it.id == agent }?.delegation
                if (delegation != null) openChat(delegation.chatId) else openedSubagent = agent
            }
        }
    val currentSubagents = remember(timeline) { currentSubagents(timeline) }
    val subagentBar =
        !fullscreen && currentSubagents.any { it.state(active) == SubagentState.RUNNING }
    // The plan of the current request stays in sight above the composer while the agent follows
    // it; only its latest card is live.
    val following = running && !chat.exclusiveSuspended
    val requestPlan =
        remember(timeline, following) { if (following) currentPlan(timeline) else null }
    val livePlan =
        remember(timeline, requestPlan) {
            if (requestPlan == null) null else timeline.lastOrNull { it.plan != null }?.key
        }
    val working =
        remember(
            live.events,
            running,
            waitingTasks,
            currentSubagents,
            chat?.agentName,
            chat?.run?.startedAt,
        ) {
            if (!running) null
            else
                waitingTasks?.let {
                    waitingStep(it)
                        .copy(subagents = waitingForSubagents(live.events, currentSubagents))
                } ?: workingStep(live.events, chat?.agentName.orEmpty(), chat?.run?.startedAt)
        }
    val conversationError = conversationError(chat)
    val error = conversationError?.takeUnless {
        quota != null && (isQuotaMessage(it) || it == quota.message)
    }
    val errorShown = remember(chat, live.events) { failureShown(chat, live.events) }
    var errorDismissed by
        remember(error, chat?.id, chat?.run?.id, chat?.run?.finishedAt) { mutableStateOf(false) }
    val selectedAgent = state.agents.find { it.id == (chat?.agentId ?: agent) }
    val currentProvider = chat?.run?.snapshot?.agent?.provider ?: selectedAgent?.provider ?: "codex"
    val chosenProvider = provider.ifBlank { currentProvider }
    val switchingProvider = chat?.run != null && chosenProvider != currentProvider
    val conversationAssistant = chat?.run?.snapshot?.agent
    val inheritConversation = chosenProvider == conversationAssistant?.provider
    val inheritedAssistant =
        if (inheritConversation) conversationAssistant
        else selectedAgent?.takeIf { it.provider == chosenProvider }
    val defaultModel = inheritedAssistant?.model.orEmpty()
    val defaultReasoning = inheritedAssistant?.reasoning.orEmpty()
    val defaultSpeed = inheritedAssistant?.speed.orEmpty()
    fun chooseProvider(value: String) {
        provider = value
        model = ""
        reasoning = ""
        speed = ""
    }
    val skillOptions =
        remember(state.skills, selectedAgent, chat?.projectId, project, chat == null) {
            chatSkills(state.skills, selectedAgent, if (chat != null) chat.projectId else project)
        }
    val skillNames = remember(skillOptions) { skillOptions.map { it.name }.toSet() }
    val mention =
        if (!composer.selection.collapsed) null else mentionAt(draft, composer.selection.start)
    // Moving to another `$` token forgets an earlier dismissal.
    LaunchedEffect(mentionAt(draft, composer.selection.start)?.start) { dismissedMention = null }
    val suggestions =
        if (mention == null || mention.start == dismissedMention) emptyList()
        else matchSkills(skillOptions, mention.query)
    val showSkills =
        mention != null &&
            mention.start != dismissedMention &&
            (suggestions.isNotEmpty() || mention.query.firstOrNull()?.isDigit() != true)
    BackHandler(selected && showSkills) { dismissedMention = mention?.start }
    LaunchedEffect(selected && showSkills) {
        if (selected && showSkills) {
            try {
                vm.refreshSkills()
            } catch (error: Exception) {
                if (error is kotlinx.coroutines.CancellationException) throw error
                // Keep the loaded catalog usable when the connection is unavailable.
            }
        }
    }
    val projects =
        state.projects.filter {
            selectedAgent?.access?.projects == null ||
                it.id in selectedAgent.access.projects.orEmpty()
        }
    val delivery = chatDelivery(chat, live.events, outgoing)
    val pending = delivery.queued
    val questions = chat?.questions.orEmpty().filter { it.status == "pending" }
    // Files over the limits stay visible as refused cards; the others join the draft.
    fun attach(
        uris: List<android.net.Uri>,
        mediaType: String? = null,
        staged: () -> Unit = {},
    ) {
        if (uris.isNotEmpty())
            vm.perform {
                try {
                    require(attachments.size + uris.size <= 8) {
                        "Vous pouvez joindre jusqu’à 8 fichiers."
                    }
                    val accepted = mutableListOf<DraftAttachment>()
                    val refused = mutableListOf<RejectedAttachment>()
                    var total = attachments.sumOf { it.attachment.size }
                    try {
                        for (uri in uris) {
                            val draft =
                                try {
                                    files.stage(uri, mediaType)
                                } catch (e: AttachmentRejected) {
                                    refused += e.rejected
                                    continue
                                }
                            if (total + draft.attachment.size > MAX_MESSAGE_ATTACHMENT_BYTES) {
                                files.discard(listOf(draft))
                                refused +=
                                    RejectedAttachment(
                                        draft.attachment.id,
                                        draft.attachment.name,
                                        draft.attachment.mediaType,
                                        "Dépasse 40 Mo pour le message",
                                    )
                                continue
                            }
                            total += draft.attachment.size
                            accepted += draft
                        }
                    } catch (e: Exception) {
                        files.discard(accepted)
                        throw e
                    }
                    attachments = attachments + accepted
                    rejected = rejected + refused
                } finally {
                    staged()
                }
            }
    }
    fun removeAttachment(key: String) {
        vm.files.discard(attachments.filter { it.attachment.id == key })
        attachments = attachments.filter { it.attachment.id != key }
        uploadErrors.remove(key)
    }
    fun clearDraft() {
        vm.files.discard(attachments)
        attachments = emptyList()
        rejected = emptyList()
        uploadErrors.clear()
        composer.clearText()
        editing = null
        submissionKey = ""
        submissionId = ""
    }
    fun send(mode: String) {
        vm.perform {
            val chatId =
                id
                    ?: createdId
                    ?: api.send<Chat>(
                            "POST",
                            "/chats",
                            buildJsonObject {
                                put("agentId", agent)
                                put("projectId", project.ifEmpty { null })
                            },
                        )
                        .id
                        .also { createdId = it }
            for (entry in attachments.toList()) if (
                entry.localPath != null && entry.attachment.chatId != chatId
            ) {
                val key = entry.attachment.id
                uploadErrors.remove(key)
                uploadProgress[key] = 0f
                val uploaded =
                    try {
                        api.upload(
                            "/chats/${segment(chatId)}/attachments/${segment(key)}?name=${segment(entry.attachment.name)}",
                            File(entry.localPath),
                        ) {
                            uploadProgress[key] = it
                        }
                    } catch (e: Exception) {
                        if (e !is kotlinx.coroutines.CancellationException)
                            uploadErrors[key] = e.message ?: "Réseau indisponible"
                        throw e
                    } finally {
                        uploadProgress.remove(key)
                    }
                attachments = attachments.map {
                    if (it.attachment.id == uploaded.id) it.copy(attachment = uploaded) else it
                }
            }
            val selectedModel = model.ifBlank { defaultModel }
            val selectedReasoning = reasoning.ifBlank {
                if (model.isBlank()) defaultReasoning else ""
            }
            // The speed survives a model change, so it is inherited even with a new model.
            val selectedSpeed = speed.ifBlank { defaultSpeed }
            val content = buildJsonObject {
                put("text", draft.trim())
                put("mode", mode)
                put("provider", chosenProvider)
                put("model", selectedModel)
                put("reasoning", selectedReasoning)
                put("speed", selectedSpeed)
                put(
                    "attachmentIds",
                    wireJson.encodeToJsonElement(attachments.map { it.attachment.id }),
                )
            }
            val key = "$chatId/${editing.orEmpty()}/$content"
            if (key != submissionKey) {
                submissionKey = key
                submissionId = UUID.randomUUID().toString()
            }
            val payload = JsonObject(content + ("id" to JsonPrimitive(editing ?: submissionId)))
            if (editing == null)
                outgoing =
                    ChatMessage(
                        submissionId,
                        chatId,
                        draft.trim(),
                        selectedModel,
                        selectedReasoning,
                        mode,
                        "sending",
                        System.currentTimeMillis(),
                        attachments.map { it.attachment },
                        provider = chosenProvider,
                        speed = selectedSpeed,
                    )
            try {
                api.request(
                    if (editing == null) "POST" else "PUT",
                    "/chats/${segment(chatId)}/messages" +
                        (editing?.let { "/${segment(it)}" } ?: ""),
                    payload,
                )
            } catch (e: Exception) {
                outgoing = null
                throw e
            }
            clearDraft()
            if (id == null) openChat(chatId)
        }
    }
    fun edit(message: ChatMessage) {
        if (draft.isNotBlank() || attachments.isNotEmpty() || rejected.isNotEmpty()) {
            vm.report(
                IllegalStateException(
                    "Envoyez ou effacez votre brouillon avant de modifier la file d’attente."
                )
            )
            return
        }
        editing = message.id
        composer.setTextAndPlaceCursorAtEnd(message.text)
        provider = message.provider.ifBlank { currentProvider }
        model = message.model
        reasoning = message.reasoning
        speed = message.speed
        attachments = message.attachments.map { DraftAttachment(it) }
    }
    fun steerQueued(message: ChatMessage) {
        vm.perform {
            api.request(
                "PUT",
                "/chats/${segment(message.chatId.ifBlank { id.orEmpty() })}/messages/${segment(message.id)}",
                buildJsonObject {
                    put("id", message.id)
                    put("text", message.text)
                    put("mode", "steer")
                    put("provider", message.provider)
                    put("model", message.model)
                    put("reasoning", message.reasoning)
                    put("speed", message.speed)
                    put(
                        "attachmentIds",
                        wireJson.encodeToJsonElement(message.attachments.map { it.id }),
                    )
                },
            )
        }
    }
    fun setPaused(current: Chat, paused: Boolean) {
        vm.perform {
            api.request(
                "POST",
                "/chats/${segment(current.id)}/pause",
                buildJsonObject { put("paused", paused) },
            )
        }
    }
    fun togglePause(current: Chat) = setPaused(current, !current.paused)
    // One callback for every wait card, so a new page state does not invalidate their rows.
    val latestChat by rememberUpdatedState(chat)
    val busy by rememberUpdatedState(state.busy)
    // Pausing, not toggling: a snapshot that turned paused meanwhile must not resume it.
    val pauseWait = remember { { if (!busy) latestChat?.let { setPaused(it, true) } } }
    val followGesture = rememberHistoryFollowGesture(listState, follow) { follow = it }
    FollowHistoryTail(
        listState,
        positionReady && follow && !gallery && !live.catchingUp,
        live.cursor,
        rendering,
        followGesture,
        lastContentIndex = timeline.lastIndex,
    )
    val agentName = chat?.agentName?.ifBlank { null } ?: selectedAgent?.name ?: "Agent"
    val reuse =
        remember(chat?.id) {
            chat?.id?.let { chatId ->
                AttachmentReuse(chatId) { attachment ->
                    if (vm.state.value.busy) return@AttachmentReuse

                    try {
                        attachments = reuseAttachment(attachments, attachment)
                    } catch (e: IllegalArgumentException) {
                        vm.report(e)
                    }
                }
            }
        }
    CompositionLocalProvider(
        LocalSkillNames provides skillNames,
        LocalAttachmentReuse provides reuse,
    ) {
        ArtifactLinkHost(vm, live.state?.artifacts.orEmpty()) {
            Column(Modifier.fillMaxSize()) {
                if (!fullscreen) {
                    val conversation =
                        conversationState(
                            chat,
                            agentName,
                            waitingTasks,
                            state.installation?.name,
                            newChat = id == null,
                        )
                    ConversationHeader(
                        title =
                            chat?.title
                                ?: conversations.state?.chats?.find { it.id == id }?.title
                                ?: if (id == null) "Nouvelle conversation" else "Conversation",
                        agent = agentName,
                        agentKey = chat?.agentId ?: agent,
                        status = conversation.status,
                        wakeup = scheduledWakeup(chat),
                        requester = chat?.requester,
                        openRequester = {
                            persistDraft()
                            openChat(it)
                        },
                        live = conversation.live,
                        harness = chat?.provider,
                        liveSince = waitingTasks?.since ?: chat?.run?.startedAt,
                        route = connectionRoute(state.installation, state.transportRoute),
                        back = {
                            persistDraft()
                            back()
                        },
                        choose = {
                            persistDraft()
                            choosing = true
                        },
                    ) {
                        val files = live.state?.artifacts?.size ?: 0
                        if (id != null) {
                            Box {
                                RoundAction("Fichiers · $files", CairnIcons.Layers, size = 40.dp) {
                                    gallery = true
                                }
                                if (files > 0)
                                    Box(
                                        Modifier.align(Alignment.TopEnd)
                                            .padding(top = 2.dp, end = 2.dp)
                                            .size(18.dp)
                                            .clip(androidx.compose.foundation.shape.CircleShape)
                                            .background(MaterialTheme.colorScheme.primary),
                                        contentAlignment = Alignment.Center,
                                    ) {
                                        Text(
                                            if (files > 99) "99+" else files.toString(),
                                            style = MaterialTheme.typography.labelSmall,
                                            color = MaterialTheme.colorScheme.onPrimary,
                                        )
                                    }
                            }
                        }
                        Box {
                            ActionIcon("Options de la conversation", CairnIcons.More) {
                                menu = true
                            }
                            DropdownMenu(menu, { menu = false }) {
                                DropdownMenuItem(
                                    text = { Text("Nouvelle conversation") },
                                    leadingIcon = { Icon(CairnIcons.Plus, null) },
                                    onClick = {
                                        menu = false
                                        persistDraft()
                                        create()
                                    },
                                )
                                DropdownMenuItem(
                                    text = { Text("Connexion") },
                                    leadingIcon = {
                                        ConnectionDot(
                                            connectionRoute(
                                                state.installation,
                                                state.transportRoute,
                                            ),
                                            description = null,
                                        )
                                    },
                                    onClick = {
                                        menu = false
                                        connection = true
                                    },
                                )
                                if (chat != null)
                                    DropdownMenuItem(
                                        text = { Text("Détails de la conversation") },
                                        leadingIcon = { Icon(Icons.Default.Info, null) },
                                        onClick = {
                                            menu = false
                                            details = true
                                        },
                                    )
                                DropdownMenuItem(
                                    text = { Text("Suivre les réponses") },
                                    leadingIcon = { Icon(CairnIcons.Bottom, null) },
                                    trailingIcon = { if (follow) Icon(CairnIcons.Check, "Activé") },
                                    onClick = {
                                        menu = false
                                        follow = !follow
                                    },
                                )
                                DropdownMenuItem(
                                    text = { Text("Plein écran") },
                                    onClick = {
                                        menu = false
                                        fullscreen = true
                                    },
                                )
                                chat?.let { current ->
                                    // Resuming needs access; reading and stopping do not.
                                    if (!(readOnly && current.paused))
                                        DropdownMenuItem(
                                            text = {
                                                Text(
                                                    if (current.paused) "Reprendre"
                                                    else "Mettre en pause"
                                                )
                                            },
                                            leadingIcon = {
                                                Icon(
                                                    if (current.paused) Icons.Default.PlayArrow
                                                    else CairnIcons.Pause,
                                                    null,
                                                )
                                            },
                                            enabled = !state.busy,
                                            onClick = {
                                                menu = false
                                                togglePause(current)
                                            },
                                        )
                                    current.runId?.let { runId ->
                                        DropdownMenuItem(
                                            text = { Text("Voir l’exécution") },
                                            leadingIcon = { Icon(CairnIcons.Terminal, null) },
                                            onClick = {
                                                menu = false
                                                openRun(runId)
                                            },
                                        )
                                    }
                                    if (active || current.wakeup != null)
                                        DropdownMenuItem(
                                            text = {
                                                Text(
                                                    "Arrêter l’agent",
                                                    color = MaterialTheme.colorScheme.error,
                                                )
                                            },
                                            leadingIcon = {
                                                Icon(
                                                    CairnIcons.Stop,
                                                    null,
                                                    tint = MaterialTheme.colorScheme.error,
                                                )
                                            },
                                            enabled = !state.busy,
                                            onClick = {
                                                menu = false
                                                stopping = true
                                            },
                                        )
                                }
                            }
                        }
                    }
                }
                if (fullscreen)
                    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            "Conversation",
                            Modifier.weight(1f).padding(start = 16.dp),
                            style = MaterialTheme.typography.titleSmall,
                        )
                        ActionIcon("Quitter le plein écran", Icons.Default.Close) {
                            fullscreen = false
                        }
                    }
                if (choosing) {
                    ModalBottomSheet(
                        onDismissRequest = { choosing = false },
                        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
                    ) {
                        Box(Modifier.fillMaxHeight(0.88f)) {
                            ConversationList(
                                conversations.state?.chats.orEmpty(),
                                id,
                                conversations.state == null,
                                conversations.error,
                                {
                                    choosing = false
                                    persistDraft()
                                    openChat(it)
                                },
                                {
                                    choosing = false
                                    persistDraft()
                                    create()
                                },
                                vm,
                            )
                        }
                    }
                }
                if (connection)
                    ConnectionSheet(
                        vm,
                        state,
                        close = { connection = false },
                        openSettings = {
                            persistDraft()
                            openSettings()
                        },
                        conversation =
                            ConversationConnection(
                                agentName,
                                chat?.projectName ?: state.projects.find { it.id == project }?.name,
                                chat?.run,
                            ),
                    )
                if (details)
                    DetailSheet("Détails de la conversation", { details = false }) {
                        Text(
                            chat?.title ?: "Nouvelle conversation",
                            style = MaterialTheme.typography.titleLarge,
                        )
                        Text(chat?.agentName ?: selectedAgent?.name.orEmpty())
                        Text(chat?.projectName ?: "Tous les projets autorisés")
                        chat?.requester?.let {
                            Text("Délégué par ${it.title.ifBlank { "Nouvelle conversation" }}")
                        }
                        Text(
                            if (chat?.paused == true) "En pause"
                            else if (chat?.exclusiveSuspended == true) "Suspendue (exclusivité)"
                            else if (chat?.run?.status == "queued") "En attente"
                            else if (waitingTasks != null) {
                                val now = rememberNow(waitingTasks.since)
                                "${waitingStep(waitingTasks).title} · ${elapsed(waitingTasks.since, now)}"
                            } else if (active) "L’agent travaille…"
                            else if (scheduledWakeup(chat) != null)
                                waitStatus(chat!!.wakeup!!, rememberNow(chat.wakeup?.id))
                            else if (chat?.run?.status in listOf("failed", "interrupted"))
                                statusLabel(chat?.run?.status.orEmpty())
                            else "Prêt"
                        )
                        Text(
                            "${live.state?.artifacts?.size ?: 0} fichiers · ${questions.size} questions en attente"
                        )
                        if (chat?.run != null) {
                            val context = contextSummary(chat.contextUsage)
                            Text("Contexte : ${context.label}", Modifier.testTag("context-usage"))
                            context.details.forEach {
                                Text(
                                    it,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                        chat
                            ?.compaction
                            ?.takeIf { it.supported && chat.lifecycle == "active" && !readOnly }
                            ?.let { compaction ->
                                TextButton(
                                    {
                                        details = false
                                        compacting = true
                                    },
                                    enabled = compaction.available,
                                ) {
                                    Text(
                                        if (compaction.running) "Compaction du contexte…"
                                        else "Compacter le contexte"
                                    )
                                }
                                compaction.reason
                                    ?.takeIf { !compaction.running }
                                    ?.let {
                                        Text(
                                            it,
                                            style = MaterialTheme.typography.bodySmall,
                                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                    }
                            }
                        chat?.run?.let {
                            if (state.isOwner) NodePlacement(vm, it)
                            Text(
                                "Dernière activité : ${date(it.finishedAt ?: it.startedAt ?: it.createdAt)}"
                            )
                        }
                    }
                if (error != null && !errorDismissed && isOpenCodePrivacyError(error))
                    OpenCodePrivacyNotice(
                        enabled = !state.busy,
                        retry =
                            if (readOnly) null else ({ if (chat != null) setPaused(chat, false) }),
                        create = if (readOnly) null else create,
                        dismiss = { errorDismissed = true },
                    )
                else if (error != null && !errorDismissed && !errorShown)
                    Row(
                        Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            error,
                            Modifier.weight(1f),
                            color = MaterialTheme.colorScheme.error,
                        )
                        ActionIcon("Masquer l’erreur", CairnIcons.Close) { errorDismissed = true }
                    }
                if (quota == null)
                    ChatRunWaitingNotice(
                        chat?.run,
                        openConnections,
                        state.isOwner,
                        state.busy,
                        {
                            chat?.let { current ->
                                vm.perform {
                                    api.request("POST", "/chats/${segment(current.id)}/stop")
                                }
                            }
                        },
                    )
                if (gallery) {
                    ModalBottomSheet(
                        onDismissRequest = { gallery = false },
                        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
                    ) {
                        Row(
                            Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(
                                "Artifacts",
                                Modifier.weight(1f),
                                style = MaterialTheme.typography.titleLarge,
                            )
                            ActionIcon("Fermer les artifacts", Icons.Default.Close) {
                                gallery = false
                            }
                        }
                        Box(Modifier.fillMaxHeight(0.85f)) {
                            Page { ArtifactsPanel(vm, live.state?.artifacts.orEmpty()) }
                        }
                    }
                }
                if (chat != null && chat.lifecycle != "active") {
                    Column(
                        Modifier.weight(1f).fillMaxWidth().padding(24.dp),
                        verticalArrangement = Arrangement.Center,
                    ) {
                        val recoverable =
                            chat.lifecycle == "trash" &&
                                (chat.purgeAt ?: 0L) > System.currentTimeMillis()
                        Text(
                            when (chat.lifecycle) {
                                "trash" ->
                                    if (recoverable) "Cette conversation est dans la corbeille."
                                    else "Effacement en cours…"
                                "purging" -> "Effacement en cours…"
                                else -> "Cette conversation est indisponible."
                            },
                            style = MaterialTheme.typography.titleLarge,
                        )
                        if (recoverable)
                            chat.purgeAt?.let { Text("Effacement prévu : ${date(it)}") }
                        chat.lifecycleError?.let {
                            Text(it, color = MaterialTheme.colorScheme.error)
                        }
                        // Restoring, like deleting, needs access to the conversation's own
                        // agent.
                        if (recoverable && canUseAgent(state.usableAgents, chat.agentId))
                            Button(
                                onClick = {
                                    vm.perform {
                                        api.request("POST", "/chats/${segment(chat.id)}/restore")
                                    }
                                },
                                enabled = !state.busy,
                            ) {
                                Text("Restaurer la conversation")
                            }
                        else if (recoverable)
                            Text(
                                "Seules les personnes ayant accès à ${chat.agentName.ifBlank { "son agent" }} peuvent la restaurer.",
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                    }
                } else
                    Column(
                        Modifier.weight(1f)
                            .widthIn(max = 840.dp)
                            .fillMaxWidth()
                            .align(Alignment.CenterHorizontally)
                    ) {
                        // Labels align with the error banner above.
                        chat?.environmentState?.let { state ->
                            EnvironmentExpirationNotice(state, chat.environmentError)
                        }
                        if (chat?.canRequestNewSession == true && !readOnly)
                            TextButton(
                                { newSession = true },
                                Modifier.padding(horizontal = 8.dp),
                            ) {
                                Text("Repartir avec une nouvelle session agent")
                            }
                        if (chat?.sessionRestartRequested == true && !readOnly)
                            Text(
                                "Écrivez un nouveau message, puis reprenez la file pour continuer.",
                                Modifier.padding(horizontal = 20.dp, vertical = 4.dp),
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        val cancelled = chat?.messages.orEmpty().filter { it.status == "cancelled" }
                        if (cancelled.isNotEmpty()) {
                            TextButton({ cancelledExpanded = !cancelledExpanded }) {
                                Text("Messages annulés (${cancelled.size})")
                            }
                            if (cancelledExpanded)
                                Column(
                                    Modifier.heightIn(max = 200.dp)
                                        .verticalScroll(rememberScrollState())
                                ) {
                                    cancelled.forEach { message ->
                                        Text(message.text)
                                        if (!readOnly)
                                            TextButton({
                                                composer.setTextAndPlaceCursorAtEnd(message.text)
                                                attachments =
                                                    message.attachments.map { DraftAttachment(it) }
                                                cancelledExpanded = false
                                            }) {
                                                Text("Copier dans le message")
                                            }
                                    }
                                }
                        }
                        if (live.catchingUp && id != null) {
                            Column(
                                Modifier.weight(1f).fillMaxWidth(),
                                verticalArrangement =
                                    Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
                                horizontalAlignment = Alignment.CenterHorizontally,
                            ) {
                                CircularProgressIndicator(Modifier.size(28.dp))
                                Text(
                                    live.error ?: live.status,
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        } else {
                            Box(Modifier.weight(1f).fillMaxWidth()) {
                                LazyColumn(
                                    Modifier.fillMaxSize()
                                        .historyVisibility(positionReady)
                                        .testTag("conversation-history")
                                        .historyFollowGesture(followGesture),
                                    state = listState,
                                    overscrollEffect = rememberHistoryOverscroll(),
                                    contentPadding = PaddingValues(16.dp),
                                    // A short history sits above the composer, so older pages
                                    // appear above it.
                                    verticalArrangement =
                                        Arrangement.spacedBy(
                                            12.dp,
                                            if (id == null) Alignment.Top else Alignment.Bottom,
                                        ),
                                ) {
                                    if (id == null)
                                        item(key = "new-conversation") {
                                            NewConversationIntro(
                                                // Members start conversations only with the
                                                // agents the owner lets them use.
                                                state.agents.filter {
                                                    canUseAgent(state.usableAgents, it.id)
                                                },
                                                agent,
                                                projects,
                                                project,
                                                enabled = createdId == null && !state.busy,
                                                chooseAgent = {
                                                    agent = it
                                                    project = ""
                                                },
                                                chooseProject = { project = it },
                                            )
                                        }
                                    items(timeline, key = { it.key }) {
                                        CompositionLocalProvider(
                                            LocalOpenSubagent provides openSubagent,
                                            LocalLiveWait provides
                                                chat?.let {
                                                    LiveWait(
                                                        liveWakeup(it),
                                                        it.paused,
                                                        pauseWait,
                                                        settled = it.run?.status == "succeeded",
                                                        busy = state.busy,
                                                        openRun = openRun,
                                                        openChat = openChat,
                                                    )
                                                },
                                        ) {
                                            TimelineRow(
                                                vm,
                                                it,
                                                chat?.agentName ?: "Cairn",
                                                rendering,
                                                hideRunning = chat?.run?.status == "running",
                                                active = active,
                                                livePlan = livePlan,
                                            )
                                        }
                                    }
                                    chat
                                        ?.takeIf {
                                            !readOnly &&
                                                (quota != null || it.codingAgentSwitch != null)
                                        }
                                        ?.let { currentChat ->
                                            item(key = "quota:${chat?.run?.id}") {
                                                ChatQuotaRecovery(
                                                    vm,
                                                    currentChat,
                                                    quota,
                                                    followConversation = { chooseProvider("") },
                                                    canManage = state.isOwner,
                                                    busy = state.busy,
                                                    openConnections = openConnections,
                                                    resume = {
                                                        chat?.let { current ->
                                                            vm.perform {
                                                                if (current.run?.status == "failed")
                                                                    api.resumeRun(
                                                                        ResumeRunInput(
                                                                            current.run.id
                                                                        )
                                                                    )
                                                                api.request(
                                                                    "POST",
                                                                    "/chats/${segment(current.id)}/pause",
                                                                    buildJsonObject {
                                                                        put("paused", false)
                                                                    },
                                                                )
                                                            }
                                                        }
                                                    },
                                                )
                                            }
                                        }
                                    if (!active && delivery.sending.isEmpty() && !live.catchingUp)
                                        chat?.run?.outcome?.let { outcome ->
                                            item(
                                                key =
                                                    "outcome:${outcome.messageId}:${outcome.reportedAt}"
                                            ) {
                                                CompletionEvidence(outcome, chat.agentName)
                                            }
                                        }
                                    items(delivery.sending, key = { "sending:${it.message.id}" }) {
                                        sending ->
                                        Column {
                                            EventRow(
                                                vm,
                                                RunEvent(
                                                    id = -1,
                                                    createdAt = sending.message.createdAt,
                                                    type = "chat.user",
                                                    text = sending.message.text,
                                                    payload =
                                                        kotlinx.serialization.json.JsonObject(
                                                            mapOf(
                                                                "text" to
                                                                    JsonPrimitive(
                                                                        questionAnswerText(
                                                                            sending.message.text,
                                                                            sending.message
                                                                                .questionId,
                                                                        )
                                                                    ),
                                                                "attachments" to
                                                                    wireJson.encodeToJsonElement(
                                                                        sending.message.attachments
                                                                    ),
                                                            )
                                                        ),
                                                ),
                                                chat?.agentName ?: "Cairn",
                                            )
                                            Text(
                                                sending.label,
                                                Modifier.align(Alignment.End).padding(end = 16.dp),
                                                style = MaterialTheme.typography.labelSmall,
                                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                            )
                                        }
                                    }
                                    if (chat?.exclusiveSuspended == true)
                                        item(key = "exclusive-suspended") {
                                            Text(
                                                "Suspendue (exclusivité) · ${chat.suspendedByExclusive?.reason ?: chat.run?.suspendedByExclusive?.reason.orEmpty()}",
                                                color = signal.warning,
                                            )
                                        }
                                    else if (
                                        working != null &&
                                            !live.catchingUp &&
                                            // The bar already shows the subagents it waits for.
                                            !(subagentBar && working.subagents)
                                    )
                                        item(key = "agent-working") { WorkingIndicator(working) }
                                }
                                HistoryStatus(
                                    live,
                                    listState,
                                    loadOlder,
                                    (live.error ?: live.status).takeIf {
                                        live.status != "En direct"
                                    },
                                )
                                HistoryBottomButton(listState, follow, "Derniers messages") {
                                    follow = true
                                }
                            }
                        }
                        if (!fullscreen) requestPlan?.let { PlanBar(it) }
                        if (subagentBar) SubagentBar(currentSubagents, active, openSubagent)
                        openedSubagent?.let { selected ->
                            SubagentSheet(
                                subagentTeams,
                                selected,
                                active,
                                openSubagent,
                            ) {
                                openedSubagent = null
                            }
                        }
                        if (questions.isNotEmpty() && readOnly)
                            Notice(
                                "${questions.size} ${if (questions.size == 1) "question attend" else "questions attendent"} une personne ayant accès à cet agent.",
                                Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
                                icon = CairnIcons.Hourglass,
                            )
                        else if (questions.isNotEmpty())
                            Surface(
                                onClick = { asking = questions.first() },
                                modifier =
                                    Modifier.fillMaxWidth()
                                        .padding(horizontal = 12.dp, vertical = 4.dp),
                                shape = RoundedCornerShape(18.dp),
                                color = signal.attentionSoft,
                            ) {
                                Row(
                                    Modifier.heightIn(min = 48.dp).padding(horizontal = 16.dp),
                                    verticalAlignment = Alignment.CenterVertically,
                                ) {
                                    Box(
                                        Modifier.size(8.dp)
                                            .clip(androidx.compose.foundation.shape.CircleShape)
                                            .background(signal.attention)
                                    )
                                    Spacer(Modifier.width(10.dp))
                                    Text(
                                        "${questions.size} ${if (questions.size == 1) "question" else "questions"} · Répondre",
                                        style = MaterialTheme.typography.titleSmall,
                                        color = MaterialTheme.colorScheme.onSurface,
                                    )
                                }
                            }
                        if (!fullscreen && noAgent)
                            Notice(
                                "Vous pouvez lire toutes les conversations de l’installation. Pour en démarrer une, demandez l’accès à un agent au propriétaire de l’installation.",
                                Modifier.padding(horizontal = 12.dp, vertical = 4.dp)
                                    .testTag("conversation-no-agent"),
                                title = "Aucun agent accessible pour l’instant",
                                icon = CairnIcons.Shield,
                                maxLines = 4,
                            )
                        if (!fullscreen && readOnly)
                            Notice(
                                "Seules les personnes ayant accès à ${chat?.agentName?.ifBlank { null } ?: "son agent"} peuvent la poursuivre, répondre à ses questions ou la supprimer. Vous pouvez toujours la mettre en pause ou l’arrêter. Demandez l’accès au propriétaire de l’installation.",
                                Modifier.padding(horizontal = 12.dp, vertical = 4.dp)
                                    .testTag("conversation-read-only"),
                                title = "Vous pouvez suivre cette conversation",
                                icon = CairnIcons.Shield,
                                maxLines = 4,
                            )
                        if (!fullscreen)
                            Surface(
                                Modifier.padding(horizontal = 12.dp, vertical = 8.dp)
                                    .testTag("conversation-composer")
                                    // Text selection and editing own horizontal gestures in the
                                    // composer.
                                    .pointerInput(Unit) {
                                        detectHorizontalDragGestures { _, _ -> }
                                    },
                                shape = RoundedCornerShape(26.dp),
                                border =
                                    androidx.compose.foundation.BorderStroke(
                                        1.dp,
                                        MaterialTheme.colorScheme.outlineVariant,
                                    ),
                                color = MaterialTheme.colorScheme.surface,
                            ) {
                                Column(Modifier.padding(4.dp)) {
                                    // Queued follow-ups open the composer itself, sharing its
                                    // border and corners.
                                    if (pending.isNotEmpty()) {
                                        QueueStrip(
                                            pending,
                                            chat
                                                ?.questions
                                                .orEmpty()
                                                .filter { q -> q.fields.any { it.secret } }
                                                .map { it.id }
                                                .toSet(),
                                            queueExpanded,
                                            { queueExpanded = !queueExpanded },
                                            busy = state.busy,
                                            canSteer = active,
                                            paused = chat?.paused == true,
                                            edit = ::edit,
                                            steer = ::steerQueued,
                                            remove = { removing = it },
                                            togglePause = { chat?.let(::togglePause) },
                                            readOnly = readOnly,
                                            models = state.modelCatalog("codex").models,
                                            attachments = {
                                                MessageAttachments(
                                                    vm,
                                                    it,
                                                    Modifier.padding(top = 8.dp),
                                                    alignment = Alignment.Start,
                                                )
                                            },
                                        )
                                        HorizontalDivider(
                                            Modifier.padding(horizontal = 12.dp),
                                            color = MaterialTheme.colorScheme.outlineVariant,
                                        )
                                    }
                                    if (attachments.isNotEmpty() || rejected.isNotEmpty())
                                        ComposerAttachments(
                                            vm,
                                            attachments,
                                            rejected,
                                            uploadProgress,
                                            uploadErrors,
                                            remove = ::removeAttachment,
                                            dismiss = { key ->
                                                rejected = rejected.filter { it.id != key }
                                            },
                                        )
                                    if (editing != null)
                                        Row {
                                            Text(
                                                "Modifier le message en attente",
                                                Modifier.weight(1f),
                                            )
                                            TextButton(onClick = ::clearDraft) { Text("Annuler") }
                                        }
                                    if (showSkills)
                                        SkillSuggestions(
                                            suggestions,
                                            mention!!.query,
                                            skillOptions.isNotEmpty(),
                                            { scope ->
                                                if (scope == "global") "Global"
                                                else
                                                    state.projects.find { it.id == scope }?.name
                                                        ?: "Projet"
                                            },
                                        ) { skill ->
                                            val inserted =
                                                insertSkill(
                                                    TextFieldValue(draft, composer.selection),
                                                    mention,
                                                    skill.name,
                                                )
                                            composer.edit {
                                                replace(0, length, inserted.text)
                                                selection = inserted.selection
                                            }
                                        }
                                    BasicTextField(
                                        composer,
                                        Modifier.fillMaxWidth()
                                            // Images pasted or inserted from the keyboard join the
                                            // attachments.
                                            .contentReceiver { content ->
                                                val images = pastedImages(content)
                                                attach(images.uris, images.mediaType)
                                                images.rest
                                            }
                                            .heightIn(min = 48.dp)
                                            .padding(
                                                top = 12.dp,
                                                bottom = 4.dp,
                                                start = 12.dp,
                                                end = 12.dp,
                                            ),
                                        textStyle =
                                            MaterialTheme.typography.bodyLarge.copy(
                                                color = MaterialTheme.colorScheme.onSurface
                                            ),
                                        cursorBrush =
                                            androidx.compose.ui.graphics.SolidColor(
                                                MaterialTheme.colorScheme.primary
                                            ),
                                        inputTransformation = InputTransformation.maxLength(50000),
                                        keyboardOptions = InputKeyboards.Sentences,
                                        outputTransformation =
                                            SkillMentionHighlight(skillNames, skillMentionStyle()),
                                        lineLimits =
                                            TextFieldLineLimits.MultiLine(maxHeightInLines = 4),
                                        enabled = !state.busy && !readOnly && !noAgent,
                                        decorator = { inner ->
                                            Box {
                                                if (draft.isEmpty())
                                                    Text(
                                                        // Any message cancels a pending wait,
                                                        // paused or not.
                                                        if (readOnly || noAgent)
                                                            "Réservé aux personnes ayant accès à cet agent"
                                                        else if (chat?.paused == true)
                                                            if (liveWakeup(chat) != null)
                                                                "Ajouter à la file — annule l’attente"
                                                            else "Ajouter à la file…"
                                                        else if (liveWakeup(chat) != null)
                                                            "Répondre maintenant annule l’attente"
                                                        else if (active) "Ajouter un message…"
                                                        else if (skillOptions.isNotEmpty())
                                                            "Votre message… $ pour les skills"
                                                        else "Votre message…",
                                                        color =
                                                            MaterialTheme.colorScheme
                                                                .onSurfaceVariant,
                                                    )
                                                inner()
                                            }
                                        },
                                    )
                                    // Message first, then one toolbar: attach, agent/model/effort,
                                    // send.
                                    Row(
                                        Modifier.fillMaxWidth(),
                                        verticalAlignment = Alignment.CenterVertically,
                                    ) {
                                        // Files and the assistant only go with a message, which
                                        // needs access to the agent.
                                        ActionIcon(
                                            "Joindre",
                                            CairnIcons.Plus,
                                            !state.busy &&
                                                !readOnly &&
                                                !noAgent &&
                                                attachments.size < 8,
                                        ) {
                                            attaching = true
                                        }
                                        if (attaching)
                                            AttachSheet(
                                                remaining = 8 - attachments.size,
                                                attach = { uris, staged ->
                                                    attach(uris, staged = staged)
                                                },
                                            ) {
                                                attaching = false
                                            }
                                        Box(Modifier.weight(1f).padding(end = 4.dp)) {
                                            ModelPicker(
                                                state.modelCatalog(chosenProvider),
                                                model,
                                                reasoning,
                                                defaultModel,
                                                defaultReasoning,
                                                inherit = inheritedAssistant != null,
                                                conversationDefault = inheritConversation,
                                                enabled = !state.busy && !readOnly && !noAgent,
                                                refresh = { vm.refreshModels(chosenProvider) },
                                                provider = chosenProvider,
                                                changeProvider = ::chooseProvider,
                                                switching = switchingProvider,
                                                speed = speed,
                                                defaultSpeed = defaultSpeed,
                                                changeSpeed = { speed = it },
                                            ) { m, r ->
                                                model = m
                                                reasoning = r
                                            }
                                        }
                                        val canSend =
                                            !state.busy &&
                                                !readOnly &&
                                                !noAgent &&
                                                rejected.isEmpty() &&
                                                uploadProgress.isEmpty() &&
                                                (draft.isNotBlank() || attachments.isNotEmpty()) &&
                                                (id == null || chat != null) &&
                                                live.error == null
                                        if (
                                            active &&
                                                editing == null &&
                                                (draft.isNotBlank() || attachments.isNotEmpty())
                                        )
                                            RoundAction(
                                                "Intervenir maintenant",
                                                CairnIcons.Steer,
                                                container =
                                                    MaterialTheme.colorScheme.primaryContainer,
                                                content =
                                                    MaterialTheme.colorScheme.onPrimaryContainer,
                                                outlined = false,
                                                size = 40.dp,
                                                enabled = canSend && !switchingProvider,
                                            ) {
                                                send(if (switchingProvider) "queue" else "steer")
                                            }
                                        if (
                                            (active || chat?.wakeup != null) &&
                                                draft.isBlank() &&
                                                attachments.isEmpty()
                                        )
                                            RoundAction(
                                                "Arrêter",
                                                CairnIcons.StopSolid,
                                                content = signal.attention,
                                                size = 40.dp,
                                                enabled = !state.busy,
                                            ) {
                                                stopping = true
                                            }
                                        else
                                            IconButton(
                                                onClick = { send("queue") },
                                                enabled = canSend,
                                                modifier =
                                                    Modifier.size(48.dp)
                                                        .testTag("conversation-send"),
                                            ) {
                                                // Keep the native touch target generous while the
                                                // visible
                                                // button stays discreet alongside the message
                                                // field.
                                                Surface(
                                                    Modifier.size(40.dp)
                                                        .testTag("conversation-send-face"),
                                                    shape =
                                                        androidx.compose.foundation.shape
                                                            .CircleShape,
                                                    color =
                                                        if (canSend)
                                                            MaterialTheme.colorScheme.primary
                                                        else
                                                            MaterialTheme.colorScheme.onSurface
                                                                .copy(alpha = 0.10f),
                                                    contentColor =
                                                        if (canSend)
                                                            MaterialTheme.colorScheme.onPrimary
                                                        else
                                                            MaterialTheme.colorScheme.onSurface
                                                                .copy(alpha = 0.38f),
                                                ) {
                                                    Box(
                                                        Modifier.fillMaxSize(),
                                                        contentAlignment = Alignment.Center,
                                                    ) {
                                                        Icon(
                                                            if (editing != null) Icons.Default.Check
                                                            else if (active || chat?.paused == true)
                                                                Icons.Default.Add
                                                            else CairnIcons.Up,
                                                            if (editing != null) "Modifier"
                                                            else if (active || chat?.paused == true)
                                                                "Ajouter à la file"
                                                            else "Envoyer",
                                                            Modifier.size(18.dp),
                                                        )
                                                    }
                                                }
                                            }
                                    }
                                }
                            }
                    }
            }
            asking?.let { selected ->
                val current = questions.find { it.id == selected.id }
                if (current == null) LaunchedEffect(selected.id) { asking = null }
                else
                    QuestionDialog(vm, state, current, questions, { asking = it }) { asking = null }
            }
            if (newSession && chat != null)
                Confirm(
                    "Nouvelle session agent ?",
                    "L’historique et les fichiers sont conservés. Votre prochain message utilisera une nouvelle session native.",
                    state.busy,
                    state.error,
                    { newSession = false },
                ) {
                    vm.perform {
                        api.request(
                            "POST",
                            "/chats/${segment(chat.id)}/new-session",
                            buildJsonObject { put("confirm", true) },
                        )
                        newSession = false
                    }
                }
            if (compacting && chat != null)
                Confirm(
                    "Compacter le contexte de l’agent ?",
                    "L’agent résume son contexte antérieur pour libérer de la place dans sa fenêtre, en utilisant son modèle comme pour un court tour. Tous les messages restent visibles ici ; ceux envoyés entre-temps attendent la fin de la compaction.",
                    state.busy,
                    state.error,
                    { compacting = false },
                ) {
                    vm.perform {
                        api.request("POST", "/chats/${segment(chat.id)}/compact")
                        compacting = false
                    }
                }
            if (stopping && chat != null)
                Confirm(
                    "Arrêter l’agent ?",
                    "La conversation sera mise en pause. Le travail déjà effectué sera conservé.",
                    state.busy,
                    state.error,
                    { stopping = false },
                ) {
                    vm.perform {
                        api.request("POST", "/chats/${segment(chat.id)}/stop")
                        stopping = false
                    }
                }
            removing?.let { message ->
                Confirm(
                    "Retirer ce message ?",
                    "Il sera supprimé de la file d’attente.",
                    state.busy,
                    state.error,
                    { removing = null },
                ) {
                    vm.perform {
                        api.request(
                            "DELETE",
                            "/chats/${segment(message.chatId)}/messages/${segment(message.id)}",
                        )
                        removing = null
                    }
                }
            }
        }
    }
}

@Composable
internal fun QuestionDialog(
    vm: CairnViewModel,
    state: Workspace,
    question: ChatQuestion,
    questions: List<ChatQuestion>,
    select: (ChatQuestion) -> Unit,
    close: () -> Unit,
) {
    val supported = question.fields.isNotEmpty() && question.fields.all { it.supportsAnswers }
    // Private answers never enter SavedState or on-disk drafts.
    var answers by remember(question.id) { mutableStateOf<Map<String, String>>(emptyMap()) }
    var selected by remember(question.id) { mutableStateOf<Map<String, List<String>>>(emptyMap()) }
    var submission by remember(question.id) { mutableStateOf("") }
    var submissionKey by remember(question.id) { mutableStateOf("") }
    Dialog(
        onDismissRequest = { if (!state.busy) close() },
        properties =
            DialogProperties(
                usePlatformDefaultWidth = false,
                securePolicy =
                    if (question.fields.any { it.secret }) SecureFlagPolicy.SecureOn
                    else SecureFlagPolicy.Inherit,
            ),
    ) {
        Scaffold(
            modifier = Modifier.imePadding(),
            topBar = { TopAppBar(title = { Text("Votre réponse") }) },
        ) { padding ->
            Column(Modifier.padding(padding).fillMaxSize()) {
                ConnectionFeedbackNotice()
                Box(Modifier.weight(1f)) {
                    Page {
                        Text(
                            if (question.blocking) "L’agent attend votre réponse."
                            else "Répondez quand vous êtes prêt."
                        )
                        if (questions.size > 1)
                            Choice(
                                "Question",
                                question.id,
                                questions.map {
                                    it.id to it.fields.firstOrNull()?.title.orEmpty().take(100)
                                },
                            ) { key ->
                                select(questions.first { it.id == key })
                            }
                        if (!supported)
                            Text(
                                "Cette question utilise un format non pris en charge. Mettez à jour l’installation Cairn, puis demandez à l’agent de reposer la question. Vous pouvez retirer cette demande ci-dessous.",
                                color = MaterialTheme.colorScheme.error,
                            )
                        if (supported)
                            question.fields.forEach { field ->
                                Text(field.title, style = MaterialTheme.typography.titleMedium)
                                field.options.forEach { option ->
                                    val checked = selected[field.id].orEmpty().contains(option.id)
                                    val toggle = {
                                        val current = selected[field.id].orEmpty()
                                        val next =
                                            if (field.mode == "single") listOf(option.id)
                                            else if (checked) current - option.id
                                            else current + option.id
                                        selected = selected + (field.id to next)
                                    }
                                    OutlinedCard(
                                        onClick = toggle,
                                        modifier = Modifier.fillMaxWidth(),
                                        colors =
                                            CardDefaults.outlinedCardColors(
                                                containerColor =
                                                    if (checked)
                                                        MaterialTheme.colorScheme.secondaryContainer
                                                    else MaterialTheme.colorScheme.surface
                                            ),
                                    ) {
                                        Row(
                                            Modifier.padding(12.dp),
                                            verticalAlignment = Alignment.CenterVertically,
                                        ) {
                                            if (field.mode == "multiple")
                                                Checkbox(checked, { toggle() })
                                            else RadioButton(checked, toggle)
                                            Column {
                                                Text(option.label)
                                                if (option.recommended)
                                                    Text(
                                                        "Recommandé",
                                                        color = MaterialTheme.colorScheme.primary,
                                                    )
                                                if (option.description.isNotBlank())
                                                    Text(
                                                        option.description,
                                                        style = MaterialTheme.typography.bodySmall,
                                                    )
                                            }
                                        }
                                    }
                                }
                                if (field.options.isNotEmpty()) {
                                    val minimum = field.minChoices ?: 1
                                    val maximum =
                                        if (field.mode == "single") 1
                                        else field.maxChoices ?: field.options.size
                                    val range =
                                        if (minimum == maximum) "$minimum"
                                        else "$minimum à $maximum"
                                    Text(
                                        if (field.mode == "multiple")
                                            "Choisissez $range options, ou répondez en texte libre."
                                        else "Choisissez une option, ou répondez en texte libre.",
                                        style = MaterialTheme.typography.bodySmall,
                                    )
                                    if (field.mode == "multiple") {
                                        val count = selected[field.id].orEmpty().size
                                        val detail =
                                            when {
                                                count > maximum ->
                                                    " · Retirez ${count - maximum} choix"
                                                count in 1 until minimum ->
                                                    " · Encore ${minimum - count} choix"
                                                else -> ""
                                            }
                                        Text(
                                            "Choix sélectionnés : $count$detail",
                                            style = MaterialTheme.typography.bodySmall,
                                            color =
                                                if (detail.isNotEmpty())
                                                    MaterialTheme.colorScheme.error
                                                else MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                    }
                                    if (selected[field.id].orEmpty().isNotEmpty())
                                        TextButton(
                                            onClick = {
                                                selected = selected + (field.id to emptyList())
                                            }
                                        ) {
                                            Text("Effacer les choix")
                                        }
                                }
                                if (field.secret)
                                    OutlinedTextField(
                                        answers[field.id].orEmpty(),
                                        { answers = answers + (field.id to it.take(10000)) },
                                        Modifier.fillMaxWidth(),
                                        label = { Text("Réponse privée") },
                                        visualTransformation = PasswordVisualTransformation(),
                                        keyboardOptions = InputKeyboards.Password,
                                    )
                                else
                                    Field(
                                        if (field.options.isEmpty()) "Réponse"
                                        else "Commentaire ou réponse libre",
                                        answers[field.id].orEmpty(),
                                        { answers = answers + (field.id to it.take(10000)) },
                                        2,
                                    )
                            }
                        state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                        TextButton(
                            onClick = {
                                vm.perform {
                                    api.request(
                                        "POST",
                                        "/chats/${segment(question.chatId)}/questions/${segment(question.id)}/cancel",
                                    )
                                    answers = emptyMap()
                                    selected = emptyMap()
                                    close()
                                }
                            },
                            enabled = !state.busy,
                        ) {
                            Text("Ce n’est plus utile")
                        }
                    }
                }
                Row(
                    Modifier.fillMaxWidth().padding(16.dp),
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    TextButton(onClick = close, enabled = !state.busy) { Text("Plus tard") }
                    Button(
                        onClick = {
                            vm.perform {
                                val values =
                                    wireJson.encodeToJsonElement(
                                        question.fields.associate { field ->
                                            field.id to
                                                QuestionAnswer(
                                                    selected =
                                                        field.options
                                                            .filter {
                                                                selected[field.id]
                                                                    .orEmpty()
                                                                    .contains(it.id)
                                                            }
                                                            .map { it.id },
                                                    comment = answers[field.id].orEmpty().trim(),
                                                )
                                        }
                                    )
                                if (submissionKey != values.toString()) {
                                    submissionKey = values.toString()
                                    submission = UUID.randomUUID().toString()
                                }
                                api.request(
                                    "POST",
                                    "/chats/${segment(question.chatId)}/questions/${segment(question.id)}/answer",
                                    buildJsonObject {
                                        put("id", submission)
                                        put("answers", values)
                                    },
                                )
                                answers = emptyMap()
                                selected = emptyMap()
                                close()
                            }
                        },
                        enabled =
                            !state.busy &&
                                supported &&
                                question.fields.all {
                                    it.accepts(selected[it.id].orEmpty(), answers[it.id].orEmpty())
                                },
                    ) {
                        Text("Envoyer la réponse")
                    }
                }
            }
        }
    }
}

/** Who and where, chosen directly on screen instead of through drop-down menus. */
@Composable
private fun NewConversationIntro(
    agents: List<Agent>,
    agent: String,
    projects: List<Project>,
    project: String,
    enabled: Boolean,
    chooseAgent: (String) -> Unit,
    chooseProject: (String) -> Unit,
) {
    Column(Modifier.fillMaxWidth().testTag("new-conversation")) {
        Text("On lance quoi ?", style = MaterialTheme.typography.headlineLarge)
        Eyebrow("Avec", Modifier.padding(top = 20.dp, bottom = 10.dp))
        androidx.compose.foundation.lazy.LazyRow(
            horizontalArrangement = Arrangement.spacedBy(10.dp)
        ) {
            items(agents, key = { it.id }) { item ->
                AgentTile(item, item.id == agent, enabled) { chooseAgent(item.id) }
            }
        }
        Eyebrow("Dans", Modifier.padding(top = 20.dp, bottom = 4.dp))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            SignalChip("Tous les projets autorisés", project.isEmpty()) {
                if (enabled) chooseProject("")
            }
            projects.forEach {
                SignalChip(it.name, project == it.id, leading = identityColor(it.id)) {
                    if (enabled) chooseProject(it.id)
                }
            }
        }
        Text(
            "Écrivez votre message ou joignez un fichier pour commencer.",
            Modifier.padding(top = 12.dp),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun AgentTile(agent: Agent, selected: Boolean, enabled: Boolean, choose: () -> Unit) {
    val shape = RoundedCornerShape(22.dp)
    Surface(
        onClick = choose,
        enabled = enabled,
        shape = shape,
        color =
            if (selected) MaterialTheme.colorScheme.surface
            else MaterialTheme.colorScheme.background,
        border =
            androidx.compose.foundation.BorderStroke(
                if (selected) 2.dp else 1.dp,
                if (selected) MaterialTheme.colorScheme.primary
                else MaterialTheme.colorScheme.outlineVariant,
            ),
        modifier =
            Modifier.width(104.dp).semantics {
                this.selected = selected
                role = androidx.compose.ui.semantics.Role.RadioButton
            },
    ) {
        Column(
            Modifier.padding(vertical = 14.dp, horizontal = 8.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Box {
                AgentAvatar(agent.name, agent.id, 48.dp)
                if (selected)
                    Box(
                        Modifier.align(Alignment.BottomEnd)
                            .offset(6.dp, 6.dp)
                            .size(20.dp)
                            .clip(androidx.compose.foundation.shape.CircleShape)
                            .background(MaterialTheme.colorScheme.surface)
                            .padding(2.dp)
                            .clip(androidx.compose.foundation.shape.CircleShape)
                            .background(MaterialTheme.colorScheme.primary),
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(
                            CairnIcons.Check,
                            null,
                            Modifier.size(11.dp),
                            tint = MaterialTheme.colorScheme.onPrimary,
                        )
                    }
            }
            Text(
                agent.name,
                Modifier.padding(top = 10.dp),
                style = MaterialTheme.typography.titleSmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                providerLabel(agent.provider),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
            )
        }
    }
}
