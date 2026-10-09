@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.serialization.json.*

internal val LocalFocusMode = compositionLocalOf<(Boolean) -> Unit> { {} }

internal data class SendingMessage(val message: ChatMessage, val label: String)

internal data class ChatDelivery(val sending: List<SendingMessage>, val queued: List<ChatMessage>)

/** [account] names the coding agent whose account the user must connect, reconnect or resume. */
internal data class ChatWaitNotice(val message: String, val account: String? = null)

internal fun chatWaitNotice(run: Run?): ChatWaitNotice? {
    if (run?.status != "queued") return null
    val reason = run.accountWaitReason?.trim()?.takeIf { it.isNotEmpty() } ?: return null
    val provider = run.accountRequired ?: return ChatWaitNotice(reason)
    return ChatWaitNotice(
        "Connectez ou réactivez un compte ${providerLabel(provider)} dans Connexions. Votre message est conservé et sera envoyé ensuite.",
        providerLabel(provider),
    )
}

/**
 * What the conversation header says: [live] while the agent runs (working, or only waiting for
 * background tasks), otherwise its [status], which names a failed or interrupted response.
 */
internal data class ConversationState(val status: String, val live: String?)

internal fun conversationState(
    chat: Chat?,
    agentName: String,
    wait: BackgroundWait?,
): ConversationState {
    if (chat == null) return ConversationState("", null)

    val run = chat.run?.status.orEmpty()
    val identity = listOfNotNull(agentName, chat.projectName).joinToString(" · ")
    val status =
        when {
            chat.paused -> "En pause"
            run == "queued" -> "En attente"
            run in listOf("failed", "interrupted") -> "${statusLabel(run)} · $identity"
            else -> identity
        }
    val tasks = wait?.tasks?.size ?: 0
    val live =
        when {
            run != "running" -> null
            tasks == 1 -> "Tâche en arrière-plan"
            tasks > 1 -> "$tasks tâches en arrière-plan"
            else -> "$agentName travaille"
        }
    return ConversationState(status, live)
}

/** The error shown above the conversation, including a response that stopped before finishing. */
internal fun conversationError(chat: Chat?): String? {
    val own = chat?.error?.takeIf { it.isNotBlank() }
    if (own != null) return own

    val run = chat?.run?.takeIf { it.status == "failed" } ?: return null
    return run.error?.trim()?.ifEmpty { null }
        ?: "La réponse s’est arrêtée avant la fin. Reprenez la conversation pour continuer."
}

@Composable
internal fun ChatWaitingNotice(
    notice: ChatWaitNotice,
    openConnections: () -> Unit,
    canManage: Boolean = true,
) {
    Surface(
        modifier =
            Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp).semantics {
                liveRegion = LiveRegionMode.Polite
            },
        color = MaterialTheme.colorScheme.secondaryContainer,
        shape = MaterialTheme.shapes.medium,
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                notice.account?.let { "Compte $it requis" } ?: "Conversation en attente",
                style = MaterialTheme.typography.titleSmall,
            )
            Text(
                if (notice.account != null && !canManage)
                    "Demandez au propriétaire de reconnecter le compte. Votre message est conservé."
                else notice.message,
                style = MaterialTheme.typography.bodyMedium,
            )
            if (notice.account != null && canManage)
                TextButton(onClick = openConnections) { Text("Ouvrir les connexions") }
        }
    }
}

/** The server's durable dispatch queue is not necessarily a waiting user message. */
internal fun chatDelivery(
    chat: Chat?,
    events: List<RunEvent>,
    outgoing: ChatMessage? = null,
): ChatDelivery {
    val acknowledged =
        events
            .filter { it.type == "chat.user" }
            .mapNotNull { (it.payload?.get("messageId") as? JsonPrimitive)?.contentOrNull }
            .toSet()
    val messages =
        chat?.messages.orEmpty().filter { it.status in setOf("queued", "sending") }.toMutableList()
    if (outgoing != null && messages.none { it.id == outgoing.id }) messages.add(outgoing)
    val active = chat?.run?.active == true
    val canStart = chat?.paused != true && (chat?.run == null || chat.run.status == "succeeded")
    val sending = mutableListOf<SendingMessage>()
    val queued = mutableListOf<ChatMessage>()
    messages.forEachIndexed { index, message ->
        if (message.id in acknowledged) return@forEachIndexed
        val local =
            message.id == outgoing?.id && chat?.messages.orEmpty().none { it.id == message.id }
        val starting = active && chat?.run?.chatExecution?.messageId == message.id
        val steering =
            active &&
                message.mode == "steer" &&
                (chat?.paused != true || message.questionId != null)
        if (local || starting || steering || (canStart && index == 0)) {
            // Private/question responses are rendered only from the server transcript.
            if (message.questionId == null)
                sending.add(
                    SendingMessage(
                        message,
                        when {
                            chat?.run?.status == "queued" ->
                                chatWaitNotice(chat.run)?.account?.let {
                                    "En attente d’un compte $it"
                                } ?: "En attente de l’agent…"
                            starting -> "Démarrage de l’agent…"
                            steering -> "Transmission à l’agent…"
                            else -> "Envoi en cours…"
                        },
                    )
                )
        } else queued.add(message)
    }
    return ChatDelivery(sending, queued)
}

internal fun outcomeLabel(outcome: TaskOutcome) =
    when (outcome.status) {
        "completed" -> "Tâche terminée"
        "blocked" -> "Tâche bloquée"
        else -> "Votre réponse est nécessaire"
    }

@Composable
internal fun CompletionEvidence(outcome: TaskOutcome, agent: String) {
    var expanded by
        rememberSaveable(outcome.messageId, outcome.reportedAt) { mutableStateOf(false) }
    val attention = outcome.status != "completed"
    val color =
        if (attention) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary
    Surface(
        modifier = Modifier.fillMaxWidth(),
        color =
            if (attention) MaterialTheme.colorScheme.errorContainer
            else MaterialTheme.colorScheme.background,
        shape = RoundedCornerShape(16.dp),
    ) {
        Column(Modifier.padding(horizontal = if (attention) 12.dp else 0.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    if (attention) Icons.Default.Info else Icons.Default.Check,
                    null,
                    Modifier.size(16.dp),
                    tint = color,
                )
                Spacer(Modifier.width(8.dp))
                Text(
                    outcomeLabel(outcome),
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.labelMedium,
                    color = color,
                )
                TextButton(onClick = { expanded = !expanded }) {
                    Text(if (expanded) "Masquer" else "Détails")
                    Icon(
                        if (expanded) CairnIcons.Down else CairnIcons.Right,
                        null,
                        Modifier.size(16.dp),
                    )
                }
            }
            if (attention) Markdown(outcome.reason)
            if (expanded) {
                if (!attention) Markdown(outcome.reason)
                outcome.evidence.forEach { evidence ->
                    HorizontalDivider(Modifier.padding(vertical = 12.dp))
                    Markdown(evidence)
                }
                Text(
                    "Rapporté par $agent · ${date(outcome.reportedAt)}",
                    Modifier.padding(vertical = 12.dp),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

/**
 * One compact bar: back, who is working and on what, live status, then the few actions that matter.
 * Secondary information belongs in the details sheet.
 */
@Composable
internal fun ConversationHeader(
    title: String,
    agent: String,
    agentKey: String,
    status: String,
    live: String?,
    liveSince: Long?,
    back: () -> Unit,
    choose: () -> Unit,
    actions: @Composable RowScope.() -> Unit,
) {
    Surface(Modifier.testTag("conversation-header"), color = MaterialTheme.colorScheme.background) {
        Column {
            Row(
                Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(start = 2.dp, end = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                ActionIcon("Retour", CairnIcons.Back, onClick = back)
                Surface(
                    onClick = choose,
                    modifier =
                        Modifier.weight(1f).semantics {
                            contentDescription = "Changer de conversation : $title"
                        },
                    color = MaterialTheme.colorScheme.background,
                    shape = RoundedCornerShape(12.dp),
                ) {
                    Row(
                        Modifier.padding(vertical = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        // The comet ring turns while the agent is running this conversation.
                        if (live != null) WorkingAvatar(agent, agentKey, 36.dp)
                        else AgentAvatar(agent, agentKey, 36.dp)
                        Spacer(Modifier.width(10.dp))
                        Column(Modifier.weight(1f)) {
                            Text(
                                title,
                                style = MaterialTheme.typography.titleMedium,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            if (live != null) {
                                val now = rememberNow(liveSince)
                                val label =
                                    listOf(live, elapsed(liveSince, now))
                                        .filter { it.isNotBlank() }
                                        .joinToString(" · ")
                                LiveChip(label, Modifier.padding(top = 2.dp))
                            } else if (status.isNotBlank())
                                Text(
                                    status,
                                    style = MaterialTheme.typography.labelMedium,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                        }
                    }
                }
                actions()
            }
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        }
    }
}

@Composable
internal fun DetailSheet(
    title: String,
    close: () -> Unit,
    content: @Composable ColumnScope.() -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = close,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        Row(
            Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(title, Modifier.weight(1f), style = MaterialTheme.typography.titleLarge)
            ActionIcon("Fermer $title", Icons.Default.Close, onClick = close)
        }
        Box(Modifier.fillMaxHeight(0.85f)) { Page(content = content) }
    }
}
