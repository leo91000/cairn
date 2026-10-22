package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import build.cairn.app.data.RunEvent
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
        // Known messages cannot become notices. Avoid serializing their complete
        // payload just to classify them again, especially during a long answer.
        if (event.id in messages) {
            if (event.type != "chat.user") {
                recovered.addAll(pending)
                pending.clear()
            }
            continue
        }

        val item = event.item()
        val success =
            event.type == "item.completed" &&
                item.string("type") == "command_execution" &&
                (item?.get("exit_code") as? JsonPrimitive)?.intOrNull == 0 &&
                item.string("status") != "failed"
        if (event.type == "turn.completed" || item.string("type") == "agent_message" || success) {
            recovered.addAll(pending)
            pending.clear()
        }
        if (presentActivity(event).connectionInterrupted()) pending.add(event.id)
    }
    return recovered
}

/** The failures a conversation shows for one attempt, and the index of its notice. */
internal class FailedAttempt(
    val reasons: MutableSet<String> = mutableSetOf(),
    var notice: Int? = null,
) {
    /** The final status and the generic adapter error only repeat the reason already shown. */
    fun repeats(event: RunEvent, reason: String) =
        event.type == "status" ||
            reason.isBlank() ||
            reason == "Conversation interrupted." ||
            reason in reasons
}

/** The reason a failure event gives, or null when the event does not report a failure. */
internal fun failureReason(event: RunEvent): String? {
    val data = event.activityData()
    val failure =
        event.type in listOf("turn.failed", "runner.failed") ||
            (event.type == "error" && data?.get("willRetry") != JsonPrimitive(true)) ||
            (event.type == "status" && event.text == "failed")
    if (!failure) return null
    return data?.get("error").asObject().string("message").ifBlank {
        data.string("message").ifBlank { if (event.type == "status") "" else event.text }
    }
}

internal fun retryTitle(event: RunEvent): String {
    val retry = event.activityData()?.get("retry").asObject()
    val attempt = (retry?.get("attempt") as? JsonPrimitive)?.intOrNull
    val limit = (retry?.get("limit") as? JsonPrimitive)?.intOrNull
    return if (attempt != null && limit != null)
        "Erreur temporaire — nouvelle tentative automatique $attempt/$limit"
    else "Erreur temporaire — nouvelle tentative automatique"
}

@Composable
internal fun ChatNotice(presentation: ActivityPresentation, retried: String = "") {
    val connection = presentation.connectionInterrupted()
    val failed = presentation.failed && !connection && retried.isBlank()
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
                if (connection) "Connexion interrompue" else retried.ifBlank { presentation.title },
                style = MaterialTheme.typography.labelLarge,
            )
            val detail = presentation.output.ifBlank { presentation.subtitle }
            if (detail.isNotBlank() && detail != presentation.title)
                Text(detail, style = MaterialTheme.typography.bodySmall)
        }
    }
}
