@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
    androidx.compose.foundation.ExperimentalFoundationApi::class,
)

package dev.leo.manager.ui

import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.foundation.background
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.content.contentReceiver
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.input.InputTransformation
import androidx.compose.foundation.text.input.TextFieldLineLimits
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.clearText
import androidx.compose.foundation.text.input.maxLength
import androidx.compose.foundation.text.input.setTextAndPlaceCursorAtEnd
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.role
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.SecureFlagPolicy
import dev.leo.manager.data.*
import java.io.File
import java.util.UUID
import kotlinx.serialization.json.*

@Composable
fun ChatsScreen(vm: LeoViewModel, state: Workspace, open: (String) -> Unit, create: () -> Unit) {
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
    vm: LeoViewModel,
) {
    var view by rememberSaveable { mutableStateOf("active") }
    var menu by remember { mutableStateOf(false) }
    var other by remember { mutableStateOf<List<Chat>>(emptyList()) }
    var revision by remember { mutableIntStateOf(0) }
    var listError by remember { mutableStateOf<String?>(null) }
    var fetching by remember { mutableStateOf(false) }
    LaunchedEffect(view, revision) {
        other = emptyList()
        if (view != "active") {
            fetching = true
            try { other = vm.api.get("/chats?view=$view") }
            catch (e: Exception) { listError = e.message }
            finally { fetching = false }
        }
    }
    // The list can sit in a bottom sheet, above the app snackbar: it hosts its own undo.
    val snackbar = remember { SnackbarHostState() }
    val trash = rememberConversationTrash(vm, snackbar) { revision++ }
    val chats = when (view) {
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
                Text("Conversations", Modifier.weight(1f), style = MaterialTheme.typography.headlineSmall)
                Box {
                    ActionIcon("Afficher les conversations", LeoIcons.More) { menu = true }
                    DropdownMenu(menu, { menu = false }) {
                        listOf("active" to "Actives", "archives" to "Archives", "trash" to "Corbeille").forEach { (key, label) ->
                            DropdownMenuItem(text = { Text(label) }, onClick = { view = key; menu = false })
                        }
                    }
                }
                ActionIcon("Nouvelle conversation", LeoIcons.Plus, onClick = create)
            }
            if (view != "active") Text(if (view == "archives") "Archives" else "Corbeille", style = MaterialTheme.typography.labelMedium)
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
                                    SwipeToTrashRow(enabled = view != "trash", onTrash = { trash.trash(chat.id) }) { swipe ->
                                        Surface(
                                            modifier = swipe,
                                            onClick = { open(chat.id) },
                                            color =
                                                if (chat.id == selected) MaterialTheme.colorScheme.primaryContainer
                                                else MaterialTheme.colorScheme.surfaceContainerLow.copy(alpha = 0f),
                                            shape = RoundedCornerShape(16.dp),
                                        ) {
                                            Row(
                                                Modifier.fillMaxWidth().heightIn(min = 64.dp).padding(horizontal = 8.dp, vertical = 8.dp),
                                                verticalAlignment = Alignment.CenterVertically,
                                            ) {
                                                val working = chat.pendingQuestions == 0 && !chat.paused && chat.status == "running"
                                                if (working) WorkingAvatar(chat.agentName.ifBlank { chat.title }, chat.agentId, 40.dp)
                                                else
                                                    AgentAvatar(
                                                        chat.agentName.ifBlank { chat.title },
                                                        chat.agentId,
                                                        40.dp,
                                                        when {
                                                            chat.pendingQuestions > 0 || chat.status in setOf("failed", "interrupted") -> AvatarBadge.ATTENTION
                                                            !chat.paused && chat.status == "queued" -> AvatarBadge.LIVE
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
                                                    if (working) WorkingLabel(statusLabel(chat.status))
                                                    else Text(
                                                        when {
                                                            chat.pendingQuestions > 0 -> "${chat.pendingQuestions} question(s) en attente"
                                                            chat.paused -> "En pause"
                                                            chat.status in listOf("running", "queued", "failed", "interrupted") ->
                                                                statusLabel(chat.status)
                                                            else -> listOfNotNull(chat.agentName.ifBlank { null }, chat.projectName).joinToString(" · ")
                                                        },
                                                        style = MaterialTheme.typography.bodySmall,
                                                        color =
                                                            if (chat.pendingQuestions > 0 || chat.status in setOf("failed", "interrupted")) signal.attention
                                                            else MaterialTheme.colorScheme.onSurfaceVariant,
                                                        maxLines = 1,
                                                        overflow = TextOverflow.Ellipsis,
                                                    )
                                                }
                                                Spacer(Modifier.width(8.dp))
                                                Text(
                                                    shortStamp(chat.updatedAt),
                                                    style = MaterialTheme.typography.labelSmall,
                                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
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

@Composable
internal fun ChatPage(
    vm: LeoViewModel,
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
) {
    val live = if (id == null) conversations else
        rememberLive(vm, state, "/chats/${segment(id)}/stream", streaming = selected)
    val chat = live.state?.chat?.takeIf { it.id == id }
    var createdId by rememberSaveable(id) { mutableStateOf<String?>(null) }
    var agent by rememberSaveable(id) { mutableStateOf(initialAgent) }
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
    var attachments by rememberForm(savedDraft.attachments)
    var editing by rememberSaveable(id) { mutableStateOf(savedDraft.editing) }
    var submissionId by rememberSaveable(id) { mutableStateOf(savedDraft.submissionId) }
    var submissionKey by rememberSaveable(id) { mutableStateOf(savedDraft.submissionKey) }
    var follow by rememberSaveable(id) { mutableStateOf(true) }
    var gallery by rememberSaveable(id) { mutableStateOf(false) }
    var menu by remember { mutableStateOf(false) }
    var fullscreen by rememberSaveable(id) { mutableStateOf(false) }
    var queueExpanded by rememberSaveable(id) { mutableStateOf(false) }
    var newSession by remember(id) { mutableStateOf(false) }
    var cancelledExpanded by rememberSaveable(id) { mutableStateOf(false) }
    BackHandler(selected && fullscreen) { fullscreen = false }
    var choosing by rememberSaveable(id) { mutableStateOf(false) }
    var details by rememberSaveable(id) { mutableStateOf(false) }
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
    val timeline =
        remember(live.events, live.state?.artifacts) {
            deliveryTimeline(
                timelineKeys.stabilize(live.history, timelineEntries(live.events, chat = true)),
                live.state?.artifacts.orEmpty(),
            )
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
    val loadOlder = rememberHistoryPaging(live, listState, positionReady && !gallery)
    val active = chat?.run?.active == true
    val selectedAgent = state.agents.find { it.id == (chat?.agentId ?: agent) }
    val currentProvider = chat?.run?.snapshot?.agent?.provider ?: selectedAgent?.provider ?: "codex"
    val chosenProvider = provider.ifBlank { currentProvider }
    val switchingProvider = chat?.run != null && chosenProvider != currentProvider
    val inheritAgentModel = chosenProvider == (selectedAgent?.provider ?: "codex")
    val defaultModel = if (inheritAgentModel) selectedAgent?.model.orEmpty() else ""
    val defaultReasoning = if (inheritAgentModel) selectedAgent?.reasoning.orEmpty() else ""
    fun chooseProvider(value: String) {
        provider = value
        model = ""
        reasoning = ""
    }
    val skillOptions =
        remember(state.skills, selectedAgent, chat?.projectId, project, chat == null) {
            chatSkills(state.skills, selectedAgent, if (chat != null) chat.projectId else project)
        }
    val skillNames = remember(skillOptions) { skillOptions.map { it.name }.toSet() }
    val mention =
        if (!composer.selection.collapsed) null
        else mentionAt(draft, composer.selection.start)
    // Moving to another `$` token forgets an earlier dismissal.
    LaunchedEffect(mentionAt(draft, composer.selection.start)?.start) { dismissedMention = null }
    val suggestions =
        if (mention == null || mention.start == dismissedMention) emptyList()
        else matchSkills(skillOptions, mention.query)
    val showSkills = mention != null && mention.start != dismissedMention &&
        (suggestions.isNotEmpty() || mention.query.firstOrNull()?.isDigit() != true)
    BackHandler(selected && showSkills) { dismissedMention = mention?.start }
    val projects =
        state.projects.filter {
            selectedAgent?.access?.projects == null ||
                it.id in selectedAgent.access.projects.orEmpty()
        }
    val delivery = chatDelivery(chat, live.events, outgoing)
    val pending = delivery.queued
    val questions = chat?.questions.orEmpty().filter { it.status == "pending" }
    fun attach(uris: List<android.net.Uri>, mediaType: String? = null) {
        if (uris.isNotEmpty())
            vm.perform {
                require(attachments.size + uris.size <= 8) {
                    "Vous pouvez joindre jusqu’à 8 fichiers."
                }
                val staged = mutableListOf<DraftAttachment>()
                try {
                    uris.forEach { staged.add(files.stage(it, mediaType)) }
                    require(
                        (attachments + staged).sumOf { it.attachment.size } <= 40L * 1024 * 1024
                    ) {
                        "Les pièces jointes doivent totaliser au maximum 40 Mo."
                    }
                    attachments = attachments + staged
                } catch (e: Exception) {
                    files.discard(staged)
                    throw e
                }
            }
    }
    val picker =
        rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) {
            attach(it)
        }
    fun clearDraft() {
        vm.files.discard(attachments)
        attachments = emptyList()
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
                val uploaded =
                    api.upload(
                        "/chats/${segment(chatId)}/attachments/${segment(entry.attachment.id)}?name=${segment(entry.attachment.name)}",
                        File(entry.localPath),
                    )
                attachments =
                    attachments.map {
                        if (it.attachment.id == uploaded.id) it.copy(attachment = uploaded) else it
                    }
            }
            val content = buildJsonObject {
                put("text", draft.trim())
                put("mode", mode)
                put("provider", chosenProvider)
                put("model", model)
                put("reasoning", reasoning)
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
                        model,
                        reasoning,
                        mode,
                        "sending",
                        System.currentTimeMillis(),
                        attachments.map { it.attachment },
                        provider = chosenProvider,
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
        if (draft.isNotBlank() || attachments.isNotEmpty()) {
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
                    put("attachmentIds", wireJson.encodeToJsonElement(message.attachments.map { it.id }))
                },
            )
        }
    }
    fun togglePause(current: Chat) {
        vm.perform {
            api.request(
                "POST",
                "/chats/${segment(current.id)}/pause",
                buildJsonObject { put("paused", !current.paused) },
            )
        }
    }
    val followGesture = rememberHistoryFollowGesture(listState, follow) { follow = it }
    FollowHistoryTail(
        listState,
        positionReady && follow && !gallery && !live.catchingUp,
        live.cursor,
        rendering,
        followGesture,
    )
    CompositionLocalProvider(LocalSkillNames provides skillNames) {
    ArtifactLinkHost(vm, live.state?.artifacts.orEmpty()) {
        Column(Modifier.fillMaxSize()) {
            if (!fullscreen) {
                val agentName = chat?.agentName?.ifBlank { null } ?: selectedAgent?.name ?: "Agent"
                ConversationHeader(
                    title = chat?.title ?: conversations.state?.chats?.find { it.id == id }?.title
                        ?: if (id == null) "Nouvelle conversation" else "Conversation",
                    agent = agentName,
                    agentKey = chat?.agentId ?: agent,
                    status = when {
                        chat == null -> ""
                        chat.paused -> "En pause"
                        chat.run?.status == "queued" -> "En attente"
                        else -> listOfNotNull(agentName, chat.projectName).joinToString(" · ")
                    },
                    live =
                        if (chat?.run?.status == "running")
                            listOf("$agentName travaille", elapsed(chat.run.startedAt)).filter { it.isNotBlank() }.joinToString(" · ")
                        else null,
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
                            RoundAction("Fichiers · $files", LeoIcons.Layers, size = 40.dp) { gallery = true }
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
                        ActionIcon("Options de la conversation", LeoIcons.More) { menu = true }
                        DropdownMenu(menu, { menu = false }) {
                            DropdownMenuItem(
                                text = { Text("Nouvelle conversation") },
                                leadingIcon = { Icon(LeoIcons.Plus, null) },
                                onClick = {
                                    menu = false
                                    persistDraft()
                                    create()
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
                                leadingIcon = { Icon(LeoIcons.Bottom, null) },
                                trailingIcon = { if (follow) Icon(LeoIcons.Check, "Activé") },
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
                                DropdownMenuItem(
                                    text = {
                                        Text(if (current.paused) "Reprendre" else "Mettre en pause")
                                    },
                                    leadingIcon = {
                                        Icon(
                                            if (current.paused) Icons.Default.PlayArrow
                                            else LeoIcons.Pause,
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
                                        leadingIcon = { Icon(LeoIcons.Terminal, null) },
                                        onClick = {
                                            menu = false
                                            openRun(runId)
                                        },
                                    )
                                }
                                if (active)
                                    DropdownMenuItem(
                                        text = { Text("Arrêter l’agent", color = MaterialTheme.colorScheme.error) },
                                        leadingIcon = { Icon(LeoIcons.Stop, null, tint = MaterialTheme.colorScheme.error) },
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
                    ActionIcon("Quitter le plein écran", Icons.Default.Close) { fullscreen = false }
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
            if (details)
                DetailSheet("Détails de la conversation", { details = false }) {
                    Text(
                        chat?.title ?: "Nouvelle conversation",
                        style = MaterialTheme.typography.titleLarge,
                    )
                    Text(chat?.agentName ?: selectedAgent?.name.orEmpty())
                    Text(chat?.projectName ?: "Tous les projets autorisés")
                    Text(
                        if (chat?.paused == true) "En pause"
                        else if (chat?.run?.status == "queued") "En attente"
                        else if (active) "L’agent travaille…" else "Prêt"
                    )
                    Text(
                        "${live.state?.artifacts?.size ?: 0} fichiers · ${questions.size} questions en attente"
                    )
                    chat?.run?.let {
                        NodePlacement(vm, it)
                        Text(
                            "Dernière activité : ${date(it.finishedAt ?: it.startedAt ?: it.createdAt)}"
                        )
                    }
                }
            chat?.error?.let {
                Text(
                    it,
                    Modifier.padding(horizontal = 20.dp),
                    color = MaterialTheme.colorScheme.error,
                )
            }
            chatWaitNotice(chat?.run)?.let { ChatWaitingNotice(it, openConnections) }
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
                        ActionIcon("Fermer les artifacts", Icons.Default.Close) { gallery = false }
                    }
                    Box(Modifier.fillMaxHeight(0.85f)) {
                        Page { ArtifactsPanel(vm, live.state?.artifacts.orEmpty()) }
                    }
                }
            }
            if (chat != null && chat.lifecycle != "active") {
                Column(Modifier.weight(1f).fillMaxWidth().padding(24.dp), verticalArrangement = Arrangement.Center) {
                    Text(when (chat.lifecycle) {
                        "trash" -> "Cette conversation est dans la corbeille."
                        "purging" -> "Effacement en cours…"
                        "restoring" -> "Restauration en cours…"
                        "archiving" -> "Archivage en cours…"
                        else -> "Cette conversation est archivée."
                    }, style = MaterialTheme.typography.titleLarge)
                    chat.purgeAt?.let { Text("Effacement prévu : ${date(it)}") }
                    if (chat.storageClass == "GLACIER") Text("La restauration peut prendre plusieurs heures.")
                    chat.lifecycleError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                    if (chat.lifecycle in setOf("trash", "archived") || (chat.lifecycle == "restoring" && chat.lifecycleError != null)) Button(onClick = {
                        vm.perform { api.request("POST", "/chats/${segment(chat.id)}/restore") }
                    }, enabled = !state.busy) { Text(if (chat.lifecycle == "restoring") "Réessayer la restauration" else "Restaurer la conversation") }
                }
            } else Column(
                Modifier.weight(1f)
                    .widthIn(max = 840.dp)
                    .fillMaxWidth()
                    .align(Alignment.CenterHorizontally)
            ) {
                if (chat?.restoredAt != null && chat.run?.status in setOf("failed", "interrupted") && !chat.sessionRestartRequested)
                    TextButton({ newSession = true }) { Text("Repartir avec une nouvelle session agent") }
                if (chat?.sessionRestartRequested == true) Text("Écrivez un nouveau message, puis reprenez la file pour continuer.")
                val cancelled = chat?.messages.orEmpty().filter { it.status == "cancelled" }
                if (cancelled.isNotEmpty()) {
                    TextButton({ cancelledExpanded = !cancelledExpanded }) { Text("Messages annulés (${cancelled.size})") }
                    if (cancelledExpanded) Column(Modifier.heightIn(max = 200.dp).verticalScroll(rememberScrollState())) {
                        cancelled.forEach { message ->
                            Text(message.text)
                            TextButton({ composer.setTextAndPlaceCursorAtEnd(message.text); attachments = message.attachments.map { DraftAttachment(it) }; cancelledExpanded = false }) { Text("Copier dans le message") }
                        }
                    }
                }
                if (live.catchingUp && id != null) {
                    Column(
                        Modifier.weight(1f).fillMaxWidth(),
                        verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
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
                                .testTag("conversation-history")
                                .historyFollowGesture(followGesture),
                            state = listState,
                            overscrollEffect = rememberHistoryOverscroll(),
                            contentPadding = PaddingValues(16.dp),
                            // A short history sits above the composer, so older pages appear above it.
                            verticalArrangement = Arrangement.spacedBy(12.dp, if (id == null) Alignment.Top else Alignment.Bottom),
                        ) {
                            if (id == null)
                                item(key = "new-conversation") {
                                    NewConversationIntro(
                                        state.agents,
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
                                TimelineRow(vm, it, chat?.agentName ?: "Leo", rendering, hideRunning = chat?.run?.status == "running")
                            }
                            if (!active && delivery.sending.isEmpty() && !live.catchingUp)
                                chat?.run?.outcome?.let { outcome ->
                                    item(key = "outcome:${outcome.messageId}:${outcome.reportedAt}") {
                                        CompletionEvidence(outcome, chat.agentName)
                                    }
                                }
                            items(delivery.sending, key = { "sending:${it.message.id}" }) { sending ->
                                Column {
                                    EventRow(
                                        vm,
                                        RunEvent(
                                            -1,
                                            sending.message.createdAt,
                                            "chat.user",
                                            sending.message.text,
                                            mapOf(
                                                "text" to JsonPrimitive(sending.message.text),
                                                "attachments" to
                                                    wireJson.encodeToJsonElement(
                                                        sending.message.attachments
                                                    ),
                                            ),
                                        ),
                                        chat?.agentName ?: "Leo",
                                    )
                                    Text(
                                        sending.label,
                                        Modifier.align(Alignment.End).padding(end = 16.dp),
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                            }
                            if (chat?.run?.status == "running" && !live.catchingUp)
                                item(key = "agent-working") {
                                    WorkingIndicator(
                                        remember(live.events, chat.agentName, chat.run.startedAt) {
                                            workingStep(live.events, chat.agentName, chat.run.startedAt)
                                        }
                                    )
                                }
                        }
                        HistoryStatus(live, listState, loadOlder, (live.error ?: live.status).takeIf { live.status != "En direct" })
                        HistoryBottomButton(listState, follow, "Derniers messages") { follow = true }
                    }
                }
                if (questions.isNotEmpty())
                    Surface(
                        onClick = { asking = questions.first() },
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp),
                        shape = RoundedCornerShape(18.dp),
                        color = signal.attentionSoft,
                    ) {
                        Row(
                            Modifier.heightIn(min = 48.dp).padding(horizontal = 16.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Box(Modifier.size(8.dp).clip(androidx.compose.foundation.shape.CircleShape).background(signal.attention))
                            Spacer(Modifier.width(10.dp))
                            Text(
                                "${questions.size} ${if (questions.size == 1) "question" else "questions"} · Répondre",
                                style = MaterialTheme.typography.titleSmall,
                                color = MaterialTheme.colorScheme.onSurface,
                            )
                        }
                    }
                if (!fullscreen)
                    Surface(
                        Modifier.padding(horizontal = 12.dp, vertical = 8.dp).testTag("conversation-composer")
                            // Text selection and editing own horizontal gestures in the composer.
                            .pointerInput(Unit) { detectHorizontalDragGestures { _, _ -> } },
                        shape = RoundedCornerShape(26.dp),
                        border = androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                        color = MaterialTheme.colorScheme.surface,
                    ) {
                        Column(Modifier.padding(4.dp)) {
                            // Queued follow-ups open the composer itself, sharing its border and corners.
                            if (pending.isNotEmpty()) {
                                QueueStrip(
                                    pending,
                                    chat?.questions.orEmpty().filter { q -> q.fields.any { it.secret } }.map { it.id }.toSet(),
                                    queueExpanded,
                                    { queueExpanded = !queueExpanded },
                                    busy = state.busy,
                                    canSteer = active,
                                    paused = chat?.paused == true,
                                    edit = ::edit,
                                    steer = ::steerQueued,
                                    remove = { removing = it },
                                    togglePause = { chat?.let(::togglePause) },
                                    attachments = { AttachmentList(vm, it) },
                                )
                                HorizontalDivider(
                                    Modifier.padding(horizontal = 12.dp),
                                    color = MaterialTheme.colorScheme.outlineVariant,
                                )
                            }
                            if (attachments.isNotEmpty())
                                Box(Modifier.heightIn(max = 140.dp)) {
                                    LazyColumn {
                                        item {
                                            AttachmentList(
                                                vm,
                                                attachments.map { it.attachment },
                                                { key ->
                                                    vm.files.discard(
                                                        attachments.filter {
                                                            it.attachment.id == key
                                                        }
                                                    )
                                                    attachments =
                                                        attachments.filter {
                                                            it.attachment.id != key
                                                        }
                                                },
                                                attachments
                                                    .mapNotNull {
                                                        it.localPath?.let { path ->
                                                            it.attachment.id to path
                                                        }
                                                    }
                                                    .toMap(),
                                            )
                                        }
                                    }
                                }
                            if (editing != null)
                                Row {
                                    Text("Modifier le message en attente", Modifier.weight(1f))
                                    TextButton(onClick = ::clearDraft) { Text("Annuler") }
                                }
                            if (showSkills)
                                SkillSuggestions(
                                    suggestions,
                                    mention!!.query,
                                    skillOptions.isNotEmpty(),
                                    { scope ->
                                        if (scope == "global") "Global"
                                        else state.projects.find { it.id == scope }?.name ?: "Projet"
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
                                    // Images pasted or inserted from the keyboard join the attachments.
                                    .contentReceiver { content ->
                                        val images = pastedImages(content)
                                        attach(images.uris, images.mediaType)
                                        images.rest
                                    }
                                    .heightIn(min = 48.dp)
                                    .padding(top = 12.dp, bottom = 4.dp, start = 12.dp, end = 12.dp),
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
                                outputTransformation = SkillMentionHighlight(skillNames, skillMentionStyle()),
                                lineLimits = TextFieldLineLimits.MultiLine(maxHeightInLines = 4),
                                enabled = !state.busy,
                                decorator = { inner ->
                                    Box {
                                        if (draft.isEmpty())
                                            Text(
                                                if (chat?.paused == true) "Ajouter à la file…"
                                                else if (active) "Ajouter un message…"
                                                else if (skillOptions.isNotEmpty()) "Votre message… $ pour les skills"
                                                else "Votre message…",
                                                color =
                                                    MaterialTheme.colorScheme.onSurfaceVariant,
                                            )
                                        inner()
                                    }
                                },
                            )
                            // Message first, then one toolbar: attach, agent/model/effort, send.
                            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                                ActionIcon(
                                    "Joindre",
                                    LeoIcons.Attach,
                                    !state.busy && attachments.size < 8,
                                ) {
                                    picker.launch(arrayOf("*/*"))
                                }
                                Box(Modifier.weight(1f).padding(end = 4.dp)) {
                                    ModelPicker(
                                        if (chosenProvider == "claude") state.claudeModels else state.models,
                                        model,
                                        reasoning,
                                        defaultModel,
                                        defaultReasoning,
                                        inherit = inheritAgentModel,
                                        enabled = !state.busy,
                                        refresh = { vm.refreshModels(chosenProvider) },
                                        provider = chosenProvider,
                                        changeProvider = ::chooseProvider,
                                        switching = switchingProvider,
                                    ) { m, r ->
                                        model = m
                                        reasoning = r
                                    }
                                }
                                val canSend =
                                    !state.busy &&
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
                                        LeoIcons.Steer,
                                        container = MaterialTheme.colorScheme.primaryContainer,
                                        content = MaterialTheme.colorScheme.onPrimaryContainer,
                                        outlined = false,
                                        size = 40.dp,
                                        enabled = canSend && !switchingProvider,
                                    ) {
                                        send(if (switchingProvider) "queue" else "steer")
                                    }
                                if (active && draft.isBlank() && attachments.isEmpty())
                                    RoundAction(
                                        "Arrêter",
                                        LeoIcons.StopSolid,
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
                                        modifier = Modifier.size(48.dp).testTag("conversation-send"),
                                    ) {
                                        // Keep the native touch target generous while the visible
                                        // button stays discreet alongside the message field.
                                        Surface(
                                            Modifier.size(40.dp).testTag("conversation-send-face"),
                                            shape = androidx.compose.foundation.shape.CircleShape,
                                            color = if (canSend) MaterialTheme.colorScheme.primary
                                                else MaterialTheme.colorScheme.onSurface.copy(alpha = 0.10f),
                                            contentColor = if (canSend) MaterialTheme.colorScheme.onPrimary
                                                else MaterialTheme.colorScheme.onSurface.copy(alpha = 0.38f),
                                        ) {
                                            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                                                Icon(
                                                    if (editing != null) Icons.Default.Check
                                                    else if (active || chat?.paused == true)
                                                        Icons.Default.Add
                                                    else LeoIcons.Up,
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
            else QuestionDialog(vm, state, current, questions, { asking = it }) { asking = null }
        }
        if (newSession && chat != null) Confirm("Nouvelle session agent ?", "L’historique et les fichiers sont conservés. Votre prochain message utilisera une nouvelle session native.", state.busy, state.error, { newSession = false }) {
            vm.perform {
                api.request("POST", "/chats/${segment(chat.id)}/new-session", buildJsonObject { put("confirm", true) })
                newSession = false
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
private fun QuestionDialog(
    vm: LeoViewModel,
    state: Workspace,
    question: ChatQuestion,
    questions: List<ChatQuestion>,
    select: (ChatQuestion) -> Unit,
    close: () -> Unit,
) {
    // Private answers never enter SavedState or on-disk drafts.
    var answers by remember(question.id) { mutableStateOf<Map<String, String>>(emptyMap()) }
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
                        question.fields.forEach { field ->
                            Text(field.title, style = MaterialTheme.typography.titleMedium)
                            field.options.forEach { option ->
                                OutlinedCard(
                                    onClick = { answers = answers + (field.id to option.label) },
                                    modifier = Modifier.fillMaxWidth(),
                                ) {
                                    Row(Modifier.padding(12.dp)) {
                                        RadioButton(
                                            answers[field.id] == option.label,
                                            { answers = answers + (field.id to option.label) },
                                        )
                                        Column {
                                            Text(option.label)
                                            if (option.description.isNotBlank())
                                                Text(
                                                    option.description,
                                                    style = MaterialTheme.typography.bodySmall,
                                                )
                                        }
                                    }
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
                                    else "Ou votre propre réponse",
                                    answers[field.id].orEmpty(),
                                    { answers = answers + (field.id to it.take(10000)) },
                                    2,
                                )
                        }
                        state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
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
                                        answers.mapValues { listOf(it.value.trim()) }
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
                                close()
                            }
                        },
                        enabled =
                            !state.busy && question.fields.all { !answers[it.id].isNullOrBlank() },
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
        androidx.compose.foundation.lazy.LazyRow(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            items(agents, key = { it.id }) { item ->
                AgentTile(item, item.id == agent, enabled) { chooseAgent(item.id) }
            }
        }
        Eyebrow("Dans", Modifier.padding(top = 20.dp, bottom = 4.dp))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            SignalChip("Tous les projets autorisés", project.isEmpty()) { if (enabled) chooseProject("") }
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
        color = if (selected) MaterialTheme.colorScheme.surface else MaterialTheme.colorScheme.background,
        border =
            androidx.compose.foundation.BorderStroke(
                if (selected) 2.dp else 1.dp,
                if (selected) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.outlineVariant,
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
                        Icon(LeoIcons.Check, null, Modifier.size(11.dp), tint = MaterialTheme.colorScheme.onPrimary)
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
