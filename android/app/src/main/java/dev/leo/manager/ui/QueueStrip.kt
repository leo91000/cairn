@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package dev.leo.manager.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.leo.manager.data.ChatAttachment
import dev.leo.manager.data.ChatMessage

/**
 * Follow-ups waiting for the agent, shown at the top of the composer. Each message keeps the full
 * width so it stays readable; the only inline action is sending the next one now. Tapping a message
 * opens its actions (edit, pause the queue, remove) and swiping left reveals removal.
 */
@Composable
internal fun QueueStrip(
    pending: List<ChatMessage>,
    privateQuestions: Set<String>,
    expanded: Boolean,
    toggle: () -> Unit,
    busy: Boolean,
    canSteer: Boolean,
    paused: Boolean,
    edit: (ChatMessage) -> Unit,
    steer: (ChatMessage) -> Unit,
    remove: (ChatMessage) -> Unit,
    togglePause: () -> Unit,
    attachments: @Composable (List<ChatAttachment>) -> Unit,
) {
    var selected by rememberSaveable { mutableStateOf<String?>(null) }
    fun steerable(message: ChatMessage) =
        canSteer &&
            message.status == "queued" &&
            message.questionId == null &&
            message.mode != "steer"
    fun preview(message: ChatMessage) =
        if (message.questionId in privateQuestions) "Réponse privée"
        else message.text.ifBlank { "Pièces jointes" }
    Column(
        Modifier.fillMaxWidth()
            .testTag("conversation-queue")
            .padding(
                start = 12.dp,
                end = 4.dp,
                top = 8.dp,
                bottom = if (pending.size > 1) 0.dp else 4.dp,
            )
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(
                LeoIcons.Clock,
                null,
                Modifier.size(14.dp),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.width(6.dp))
            Text(
                queueHeadline(paused, canSteer) +
                    if (pending.size > 1) " · ${pending.size}" else "",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Column(
            if (expanded) Modifier.heightIn(max = 260.dp).verticalScroll(rememberScrollState())
            else Modifier
        ) {
            (if (expanded) pending else pending.take(1)).forEachIndexed { index, message ->
                key(message.id) {
                    SwipeToRevealRow(
                        enabled = message.status == "queued" && !busy,
                        label = "Retirer",
                        onDelete = { remove(message) },
                    ) { swipe ->
                        Row(
                            swipe
                                .fillMaxWidth()
                                .clip(RoundedCornerShape(12.dp))
                                .clickable(onClickLabel = "Options du message en attente") {
                                    selected = message.id
                                }
                                .testTag("queued-message"),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(Modifier.weight(1f).padding(vertical = 6.dp)) {
                                Text(
                                    preview(message),
                                    style = MaterialTheme.typography.bodyMedium,
                                    maxLines = 2,
                                    overflow = TextOverflow.Ellipsis,
                                )
                                val note =
                                    when {
                                        message.status == "sending" -> "Envoi en cours…"
                                        message.mode == "steer" -> "Transmis dès que possible"
                                        message.attachments.isNotEmpty() ->
                                            "${message.attachments.size} pièce${if (message.attachments.size > 1) "s" else ""} jointe${if (message.attachments.size > 1) "s" else ""}"
                                        else -> null
                                    }
                                if (note != null)
                                    Text(
                                        note,
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                            }
                            // Only the next message gets the shortcut; the others offer it in their
                            // sheet.
                            if (index == 0 && steerable(message)) {
                                Spacer(Modifier.width(8.dp))
                                FilledTonalButton(
                                    onClick = { steer(message) },
                                    enabled = !busy,
                                    contentPadding = PaddingValues(horizontal = 12.dp),
                                    modifier = Modifier.height(36.dp),
                                ) {
                                    Icon(LeoIcons.Steer, null, Modifier.size(14.dp))
                                    Spacer(Modifier.width(4.dp))
                                    Text("Maintenant", style = MaterialTheme.typography.labelLarge)
                                }
                            }
                        }
                    }
                }
            }
        }
        if (pending.size > 1)
            TextButton(onClick = toggle, contentPadding = PaddingValues(horizontal = 0.dp)) {
                Text(
                    if (expanded) "Réduire"
                    else
                        "+ ${pending.size - 1} autre${if (pending.size > 2) "s" else ""} message${if (pending.size > 2) "s" else ""}",
                    style = MaterialTheme.typography.labelMedium,
                )
            }
    }
    pending
        .firstOrNull { it.id == selected }
        ?.let { message ->
            QueuedMessageSheet(
                message,
                preview(message),
                headline = queueHeadline(paused, canSteer),
                busy = busy,
                paused = paused,
                steer = if (steerable(message)) ({ steer(message) }) else null,
                edit =
                    if (message.status == "queued" && message.questionId == null)
                        ({ edit(message) })
                    else null,
                remove = if (message.status == "queued") ({ remove(message) }) else null,
                togglePause = togglePause,
                attachments = attachments,
                close = { selected = null },
            )
        }
}

private fun queueHeadline(paused: Boolean, working: Boolean) =
    when {
        paused -> "File en pause · rien ne part"
        working -> "Envoyé quand l’agent aura fini"
        else -> "En attente d’envoi"
    }

/** The whole message and every action on it, each one explained; removal stays last and red. */
@Composable
private fun QueuedMessageSheet(
    message: ChatMessage,
    text: String,
    headline: String,
    busy: Boolean,
    paused: Boolean,
    steer: (() -> Unit)?,
    edit: (() -> Unit)?,
    remove: (() -> Unit)?,
    togglePause: () -> Unit,
    attachments: @Composable (List<ChatAttachment>) -> Unit,
    close: () -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = close,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = MaterialTheme.colorScheme.background,
    ) {
        Column(
            Modifier.fillMaxWidth()
                .testTag("queued-message-sheet")
                .padding(horizontal = 20.dp)
                .padding(bottom = 24.dp)
        ) {
            Text(
                headline,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(8.dp))
            Surface(
                shape = RoundedCornerShape(14.dp),
                color = MaterialTheme.colorScheme.surfaceVariant,
            ) {
                Column(
                    Modifier.fillMaxWidth()
                        .heightIn(max = 220.dp)
                        .verticalScroll(rememberScrollState())
                        .padding(12.dp)
                ) {
                    Text(text, style = MaterialTheme.typography.bodyMedium)
                    if (message.attachments.isNotEmpty()) attachments(message.attachments)
                }
            }
            Spacer(Modifier.height(8.dp))
            fun then(action: () -> Unit): () -> Unit = {
                close()
                action()
            }
            steer?.let {
                SheetAction(
                    LeoIcons.Steer,
                    "Envoyer maintenant",
                    "L’agent le lit sans attendre la fin de sa tâche",
                    MaterialTheme.colorScheme.primary,
                    !busy,
                    then(it),
                )
            }
            edit?.let {
                SheetAction(
                    LeoIcons.Pencil,
                    "Modifier",
                    null,
                    MaterialTheme.colorScheme.onSurface,
                    !busy,
                    then(it),
                )
            }
            SheetAction(
                if (paused) Icons.Default.PlayArrow else LeoIcons.Pause,
                if (paused) "Reprendre la file" else "Mettre la file en pause",
                if (paused) "Les messages repartent dans l’ordre"
                else "Rien ne part tant que vous ne reprenez pas",
                MaterialTheme.colorScheme.onSurface,
                !busy,
                then(togglePause),
            )
            remove?.let {
                SheetAction(
                    Icons.Default.Delete,
                    "Retirer de la file",
                    null,
                    MaterialTheme.colorScheme.error,
                    !busy,
                    then(it),
                )
            }
            if (message.status == "sending")
                Text(
                    "Envoi en cours…",
                    Modifier.padding(top = 8.dp),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
        }
    }
}

@Composable
private fun SheetAction(
    icon: ImageVector,
    title: String,
    detail: String?,
    tint: Color,
    enabled: Boolean,
    onClick: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 56.dp)
            .clip(RoundedCornerShape(12.dp))
            .clickable(enabled = enabled, onClick = onClick)
            .padding(horizontal = 4.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, null, Modifier.size(20.dp), tint = tint)
        Spacer(Modifier.width(16.dp))
        Column {
            Text(
                title,
                style = MaterialTheme.typography.bodyLarge,
                fontWeight = FontWeight.Medium,
                color = tint,
            )
            if (detail != null)
                Text(
                    detail,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
        }
    }
}
