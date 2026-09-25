package dev.leo.manager.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.leo.manager.data.RunEvent
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.intOrNull

private val connectionSource =
    Regex("websocket|reconnecting\\.\\.\\.|reconnecting\\s+\\d+/", RegexOption.IGNORE_CASE)
private val connectionFailure =
    Regex("503|reconnect|falling back|connection.*(?:closed|failed)", RegexOption.IGNORE_CASE)

internal fun ActivityPresentation.connectionInterrupted(): Boolean =
    kind == ActivityKind.NOTICE &&
        connectionSource.containsMatchIn(raw) &&
        connectionFailure.containsMatchIn(raw)

/** Only clear retry notices when work actually continues in the same turn. */
internal fun recoveredChatConnections(events: List<RunEvent>, messages: Set<Long>): Set<Long> {
    val pending = mutableSetOf<Long>()
    val recovered = mutableSetOf<Long>()
    for (event in events) {
        if (event.type in listOf("turn.started", "turn.failed", "chat.user")) pending.clear()
        val item = event.item()
        val success =
            event.type == "item.completed" &&
                item.string("type") == "command_execution" &&
                (item?.get("exit_code") as? JsonPrimitive)?.intOrNull == 0 &&
                item.string("status") != "failed"
        if (
            event.type == "turn.completed" ||
                item.string("type") == "agent_message" ||
                (event.id in messages && event.type != "chat.user") ||
                success
        ) {
            recovered.addAll(pending)
            pending.clear()
        }
        if (presentActivity(event).connectionInterrupted()) pending.add(event.id)
    }
    return recovered
}

@Composable
internal fun ChatNotice(presentation: ActivityPresentation) {
    val connection = presentation.connectionInterrupted()
    val failed = presentation.failed && !connection
    Surface(
        modifier = Modifier.fillMaxWidth(),
        color =
            if (failed) MaterialTheme.colorScheme.errorContainer
            else MaterialTheme.colorScheme.surfaceContainerLow,
        contentColor =
            if (failed) MaterialTheme.colorScheme.onErrorContainer
            else MaterialTheme.colorScheme.onSurface,
        shape = MaterialTheme.shapes.small,
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                if (connection) "Connexion interrompue" else presentation.title,
                style = MaterialTheme.typography.labelLarge,
            )
            val detail = presentation.output.ifBlank { presentation.subtitle }
            if (detail.isNotBlank() && detail != presentation.title)
                Text(detail, style = MaterialTheme.typography.bodySmall)
        }
    }
}
