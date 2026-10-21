package build.cairn.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.serialization.json.*

private val quotaMessage =
    Regex(
        "^(?:you['’]ve hit your (?:(?:usage|session|weekly|spend|individual spend) )?limit|you have hit your (?:usage )?limit|usage limit (?:has been )?(?:reached|exceeded)|claude ai usage limit reached|(?:5-hour|session|weekly|opus) limit reached)\\b",
        RegexOption.IGNORE_CASE,
    )

internal fun isQuotaMessage(message: String?) =
    quotaMessage.containsMatchIn(message.orEmpty().trim())

internal data class ChatQuota(
    val provider: String,
    val message: String,
    val spend: Boolean,
    val reset: String?,
    val automatic: Boolean,
)

private fun quotaFailure(event: RunEvent): String {
    if (event.type !in listOf("turn.failed", "error")) return ""
    val data = event.activityData()
    if (data?.get("willRetry") == JsonPrimitive(true)) return ""
    val error = if (event.type == "turn.failed") data?.get("error").asObject() else data
    val message = error.string("message").ifBlank { event.text }
    val exhausted =
        error.string("code") == "usage_limit_reached" ||
            error.string("codexErrorInfo") == "usageLimitExceeded" ||
            isQuotaMessage(message)
    return if (exhausted) message.ifBlank { "Usage limit reached" } else ""
}

/** A saved failure needs manual resume; only the manager's explicit wait is automatic. */
internal fun chatQuota(run: Run?, events: List<RunEvent>): ChatQuota? {
    if (run == null) return null
    val current = events.filter { it.runId == run.id }
    val failure = current.lastOrNull { quotaFailure(it).isNotEmpty() }
    val failureMessage = failure?.let(::quotaFailure).orEmpty()
    val waiting =
        run.active &&
            Regex(
                    "^(?:Usage exhausted\\.|Waiting for a .+ account with available usage\\.)",
                    RegexOption.IGNORE_CASE,
                )
                .containsMatchIn(run.accountWaitReason.orEmpty())
    val savedQuota =
        run.status == "failed" &&
            (isQuotaMessage(run.error) ||
                (failureMessage.isNotEmpty() && failureMessage == run.error))
    if (!waiting && !savedQuota) return null
    val message = failureMessage.ifEmpty { run.error?.takeIf(::isQuotaMessage).orEmpty() }
    return ChatQuota(
        providerLabel(run.snapshot.agent.provider),
        message,
        message.contains("spend limit", ignoreCase = true),
        Regex("\\bresets\\b(.*)$", RegexOption.IGNORE_CASE)
            .find(message)
            ?.groupValues
            ?.get(1)
            ?.trim()
            ?.removeSuffix(".")
            ?.ifEmpty { null },
        // Pausing the follow-up queue does not pause an execution's capacity wait.
        waiting,
    )
}

private data class QuotaEcho(var text: String = "", val ids: MutableList<Long> = mutableListOf())

private data class QuotaTurn(
    var exhausted: Boolean = false,
    var legacyTool: Boolean = false,
    val causes: MutableSet<String> = mutableSetOf(),
    val messages: MutableMap<String, QuotaEcho> = mutableMapOf(),
)

/** Keep recovered quota bookkeeping in run diagnostics, rather than restoring chat errors. */
internal fun quotaHiddenEvents(events: List<RunEvent>): Set<Long> {
    val hidden = mutableSetOf<Long>()
    val turns = mutableMapOf<String, QuotaTurn>()
    for (event in events) {
        if (event.runId !in turns || event.type in listOf("turn.started", "chat.user"))
            turns[event.runId] = QuotaTurn()
        val turn = turns.getValue(event.runId)
        val item = event.item()
        val legacy = event.activityData() == null
        val legacyMessage = legacy && event.type == "item.completed" && !turn.legacyTool
        if (legacy && event.type == "item.started") turn.legacyTool = true
        if (legacy && event.type == "item.completed") turn.legacyTool = false
        if (item.string("type") == "agent_message" || legacyMessage) {
            val key = item.string("id").ifBlank { event.id.toString() }
            val message = turn.messages.getOrPut(key) { QuotaEcho() }
            message.text = item.string("text").ifBlank { event.text }
            message.ids.add(event.id)
            if (turn.exhausted && (isQuotaMessage(message.text) || message.text in turn.causes))
                hidden.addAll(message.ids)
        }
        val cause = quotaFailure(event)
        if (cause.isNotEmpty()) {
            turn.exhausted = true
            turn.causes.add(cause)
            hidden.add(event.id)
            turn.messages.values
                .filter { isQuotaMessage(it.text) || it.text in turn.causes }
                .forEach { hidden.addAll(it.ids) }
        }
        if (turn.exhausted && event.type == "status" && event.text == "failed") hidden.add(event.id)
    }
    return hidden
}

/** Process lifecycle events first so subagents and plans retain their real stopped state. */
internal fun quotaTimeline(
    entries: List<TimelineEntry>,
    events: List<RunEvent>,
): List<TimelineEntry> {
    val hidden = quotaHiddenEvents(events)
    return entries.filterNot { entry ->
        val event = entry.events.singleOrNull()
        event != null &&
            (entry.message || entry.notice) &&
            (event.id in hidden || entry.sourceIds[event.id].orEmpty().any { it in hidden })
    }
}

@Composable
internal fun ChatQuotaCard(
    quota: ChatQuota,
    canManage: Boolean,
    busy: Boolean,
    openConnections: () -> Unit,
    resume: () -> Unit,
) {
    var expanded by rememberSaveable(quota.message) { mutableStateOf(false) }
    val tone = signal.warning
    val surface = signal.warningSoft
    Surface(
        Modifier.fillMaxWidth().testTag("chat-quota").semantics {
            liveRegion = LiveRegionMode.Polite
        },
        color = surface,
        contentColor = MaterialTheme.colorScheme.onSurface,
        shape = RoundedCornerShape(20.dp),
        border = BorderStroke(1.dp, tone.copy(alpha = 0.3f)),
    ) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Icon(CairnIcons.Clock, null, Modifier.size(24.dp), tint = tone)
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        "Quota ${quota.provider} atteint",
                        style = MaterialTheme.typography.titleMedium,
                        color = tone,
                    )
                    Text(
                        if (quota.spend) "La limite de dépense du compte est atteinte."
                        else "La limite d’usage du compte est atteinte.",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
            quota.reset?.let {
                Text(
                    "Réinitialisation annoncée : $it",
                    style = MaterialTheme.typography.bodyMedium,
                    color = tone,
                )
            }
            Text(
                if (quota.automatic) "Reprise automatique dès qu’un compte est disponible."
                else "Reprenez la conversation quand un compte est disponible.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (canManage)
                Button(
                    onClick = openConnections,
                    enabled = !busy,
                    colors =
                        ButtonDefaults.buttonColors(containerColor = tone, contentColor = surface),
                ) {
                    Text("Gérer les comptes")
                }
            else
                Text(
                    "Demandez au propriétaire de l’installation de vérifier les comptes.",
                    style = MaterialTheme.typography.bodySmall,
                )
            if (!quota.automatic)
                TextButton(onClick = resume, enabled = !busy) { Text("Reprendre la conversation") }
            if (quota.message.isNotBlank()) {
                TextButton(onClick = { expanded = !expanded }) {
                    Icon(
                        if (expanded) CairnIcons.Down else CairnIcons.Right,
                        null,
                        Modifier.size(16.dp),
                    )
                    Spacer(Modifier.width(6.dp))
                    Text("Détails techniques", color = tone)
                }
                if (expanded) Text(quota.message, style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}
