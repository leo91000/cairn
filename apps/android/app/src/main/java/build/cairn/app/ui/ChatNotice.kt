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

/** A reported error, diagnostic or output about a connection the provider is restoring. */
internal fun connectionNotice(
    event: RunEvent,
    presentation: ActivityPresentation = presentActivity(event),
): Boolean =
    event.type in listOf("error", "diagnostic", "item.completed") &&
        presentation.connectionInterrupted()

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
        if (connectionNotice(event)) pending.add(event.id)
    }
    return recovered
}

/** A failed attempt's reasons, the index of its notice and how often Cairn retried its chain. */
internal class FailedAttempt(
    val reasons: MutableSet<String> = mutableSetOf(),
    var notice: Int? = null,
    var retries: Int? = null,
    /** Its turn or run failed, beyond a reported error. */
    var ended: Boolean = false,
    /** Cairn scheduled a retry that has not started yet. */
    var pending: Boolean = false,
) {
    /**
     * A blank reason only repeats the reason already shown, and the generic adapter error a turn
     * failure; after an error report, it says the turn ended. A final status may give the reason
     * truncated.
     */
    fun repeats(event: RunEvent, reason: String) =
        reason.isBlank() ||
            reason in reasons ||
            (event.type == "status" && reasons.any { sameReason(it, reason) }) ||
            (reason == "Conversation interrupted." && ended)
}

/** Whether a reason repeats one already given; a run's recorded error may be truncated. */
internal fun sameReason(given: String, reason: String) =
    given == reason || (reason.isNotEmpty() && given.startsWith(reason))

// Adapters report a stop that Cairn requested as a failed turn.
private val STOP_ECHOES =
    setOf("Conversation stopped.", "Claude execution stopped.", "OpenCode execution stopped.")

/** An adapter's report of a stop Cairn requested; the terminal status that follows says so. */
internal fun stopEcho(event: RunEvent, reason: String? = failureReason(event)) =
    event.type != "status" && reason in STOP_ECHOES

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

/** Cairn's retry number for this failure, counted across the chain, and its limit. */
internal fun retryProgress(event: RunEvent, previous: Int?): Pair<Int, Int?> {
    val retry = event.activityData()?.get("retry").asObject()
    val attempt = (retry?.get("attempt") as? JsonPrimitive)?.intOrNull ?: ((previous ?: 0) + 1)
    return attempt to (retry?.get("limit") as? JsonPrimitive)?.intOrNull
}

internal fun retryTitle(attempt: Int, limit: Int?): String =
    if (limit != null) "Erreur temporaire — nouvelle tentative automatique $attempt/$limit"
    else "Erreur temporaire — nouvelle tentative automatique"

internal fun retriedFailureTitle(retries: Int): String =
    if (retries == 1) "L’agent a rencontré une erreur après 1 nouvelle tentative automatique"
    else "L’agent a rencontré une erreur après $retries nouvelles tentatives automatiques"

@Composable
internal fun ChatNotice(
    presentation: ActivityPresentation,
    title: String = "",
    retrying: Boolean = false,
    connection: Boolean = presentation.connectionInterrupted(),
) {
    // A retry title says more than the connection pattern of its reason.
    val connection = connection && title.isBlank()
    val failed = presentation.failed && !connection && !retrying
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
                if (connection) "Connexion interrompue" else title.ifBlank { presentation.title },
                style = MaterialTheme.typography.labelLarge,
            )
            val detail = presentation.output.ifBlank { presentation.subtitle }
            if (detail.isNotBlank() && detail != presentation.title)
                Text(detail, style = MaterialTheme.typography.bodySmall)
        }
    }
}
