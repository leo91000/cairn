package build.cairn.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import java.util.UUID
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
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

/** A live account wait that recovers automatically, without reconnecting an account. */
internal fun isQuotaWait(status: String, reason: String?, accountRequired: String?): Boolean =
    status in listOf("running", "queued") &&
        accountRequired.isNullOrEmpty() &&
        Regex(
                "^(?:Usage exhausted\\.|Waiting for a .+ account with available usage\\.)",
                RegexOption.IGNORE_CASE,
            )
            .containsMatchIn(reason.orEmpty())

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
    var failureMessage = ""
    // When the engine reported the limit, it said when the limit resets.
    var failureReset: Long? = null
    for (event in current) {
        val turnBoundary = event.type in listOf("chat.user", "turn.started", "turn.completed")
        val executionBoundary =
            event.type == "status" &&
                event.text in listOf("queued", "succeeded", "cancelled", "interrupted")
        if (turnBoundary || executionBoundary) {
            failureMessage = ""
            failureReset = null
        }
        val failure = quotaFailure(event)
        if (failure.isNotEmpty()) {
            // The runner repeats the engine's failure without its reset.
            val reset =
                (event.activityData()?.get("error").asObject()?.get("resetsAt") as? JsonPrimitive)
                    ?.longOrNull
            failureReset = reset ?: failureReset.takeIf { failure == failureMessage }
            failureMessage = failure
        }
    }
    val exhausted =
        run.active &&
            run.accountWaitReason.orEmpty().startsWith("Usage exhausted.", ignoreCase = true)
    val admission =
        run.active &&
            Regex("^Waiting for a .+ account with available usage\\.", RegexOption.IGNORE_CASE)
                .containsMatchIn(run.accountWaitReason.orEmpty())
    val waiting = exhausted || admission
    val savedQuota =
        run.status == "failed" &&
            (isQuotaMessage(run.error) ||
                (failureMessage.isNotEmpty() && failureMessage == run.error))
    if (!waiting && !savedQuota) return null
    // Admission has no current agent failure; saved failures have an authoritative error.
    val message = if (savedQuota) run.error.orEmpty() else if (exhausted) failureMessage else ""
    return ChatQuota(
        providerLabel(run.snapshot?.agent?.provider ?: run.provider ?: "codex"),
        message,
        message.contains("spend limit", ignoreCase = true),
        Regex("\\bresets\\b(.*)$", RegexOption.IGNORE_CASE)
            .find(message)
            ?.groupValues
            ?.get(1)
            ?.trim()
            ?.removeSuffix(".")
            ?.ifEmpty { null }
            ?: limitReset(failureReset).takeIf { it.isNotEmpty() && message == failureMessage },
        // Pausing the follow-up queue does not pause an execution's capacity wait.
        waiting,
    )
}

private data class QuotaEcho(var text: String = "", val ids: MutableList<Long> = mutableListOf())

private data class QuotaTurn(
    var exhausted: Boolean = false,
    val causes: MutableSet<String> = mutableSetOf(),
    val failures: MutableSet<String> = mutableSetOf(),
    val messages: MutableMap<String, QuotaEcho> = mutableMapOf(),
)

/**
 * Keep recovered quota bookkeeping and replies echoing a failure in run diagnostics, rather than
 * restoring chat errors.
 */
internal fun quotaHiddenEvents(events: List<RunEvent>): Set<Long> {
    val hidden = mutableSetOf<Long>()
    val turns = mutableMapOf<String, QuotaTurn>()
    for (event in events) {
        if (event.runId !in turns || event.type in listOf("turn.started", "chat.user"))
            turns[event.runId] = QuotaTurn()
        val turn = turns.getValue(event.runId)
        val item = event.item()
        if (item.string("type") == "agent_message") {
            val key = item.string("id").ifBlank { event.id.toString() }
            val message = turn.messages.getOrPut(key) { QuotaEcho() }
            message.text = item.string("text").ifBlank { event.text }
            message.ids.add(event.id)
            if (
                (turn.exhausted && (isQuotaMessage(message.text) || message.text in turn.causes)) ||
                    message.text.trim() in turn.failures
            )
                hidden.addAll(message.ids)
        }
        // An agent may also say its failure as a reply, as Claude Code does for API errors: the
        // failure notice already shows it.
        val reason = failureReason(event)?.trim()
        if (!reason.isNullOrEmpty() && reason != "Conversation interrupted.") {
            turn.failures.add(reason)
            turn.messages.values
                .filter { it.text.trim() == reason }
                .forEach { hidden.addAll(it.ids) }
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
    choices: List<CodingAgentChoice> = emptyList(),
    switching: String? = null,
    switchError: String? = null,
    switch: (String) -> Unit = {},
    cancelSwitch: () -> Unit = {},
    loading: Boolean = false,
) {
    var expanded by rememberSaveable(quota.message) { mutableStateOf(false) }
    Surface(
        Modifier.fillMaxWidth().testTag("chat-quota").semantics {
            liveRegion = LiveRegionMode.Polite
        },
        color = MaterialTheme.colorScheme.surface,
        contentColor = MaterialTheme.colorScheme.onSurface,
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column(
            Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                if (switching != null)
                    CircularProgressIndicator(
                        Modifier.padding(top = 4.dp).size(14.dp),
                        strokeWidth = 2.dp,
                    )
                else
                    Box(
                        Modifier.padding(top = 7.dp)
                            .size(6.dp)
                            .background(signal.warning, CircleShape)
                    )
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text(
                        when {
                            switching != null -> "Passage à $switching…"
                            quota.spend -> "Limite de dépense ${quota.provider} atteinte"
                            else -> "Quota ${quota.provider} atteint"
                        },
                        style = MaterialTheme.typography.titleSmall,
                    )
                    Text(
                        when {
                            switching != null -> "La conversation et les fichiers sont conservés."
                            quota.automatic ->
                                "Reprise automatique dès qu’un compte est disponible."
                            else -> "Reprenez la conversation quand un compte est disponible."
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    if (switching == null)
                        quota.reset?.let {
                            Text(
                                "Réinitialisation annoncée : $it",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                }
            }
            if (switching != null) {
                TextButton(onClick = cancelSwitch, enabled = !busy) {
                    Text("Annuler le changement")
                }
            } else if (choices.isNotEmpty() || !quota.automatic || loading) {
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    choices.forEach { choice ->
                        FilledTonalButton(onClick = { switch(choice.provider) }, enabled = !busy) {
                            Text("Continuer avec ${choice.label}")
                        }
                    }
                    if (loading && choices.isEmpty())
                        Row(
                            Modifier.padding(vertical = 8.dp),
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
                            Text(
                                "Vérification des alternatives…",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                    if (!quota.automatic)
                        TextButton(onClick = resume, enabled = !busy) {
                            Text("Reprendre la conversation")
                        }
                }
            }
            if (canManage || quota.message.isNotBlank())
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (canManage)
                        TextButton(onClick = openConnections, enabled = !busy) {
                            Text("Gérer les comptes")
                        }
                    if (quota.message.isNotBlank())
                        TextButton(onClick = { expanded = !expanded }) {
                            Icon(
                                if (expanded) CairnIcons.Down else CairnIcons.Right,
                                null,
                                Modifier.size(16.dp),
                            )
                            Spacer(Modifier.width(6.dp))
                            Text("Détails techniques")
                        }
                }
            if (expanded && quota.message.isNotBlank())
                Text(quota.message, style = MaterialTheme.typography.bodySmall)
            switchError?.let {
                Text(it, color = signal.warning, style = MaterialTheme.typography.bodySmall)
            }
            if (
                !canManage &&
                    choices.isEmpty() &&
                    switching == null &&
                    !loading &&
                    switchError == null
            )
                Text(
                    "Demandez au propriétaire de l’installation de vérifier les comptes.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
        }
    }
}

/** One explicit choice, with the manager retaining a switch across disconnects/restarts. */
@Composable
internal fun ChatQuotaRecovery(
    vm: CairnViewModel,
    chat: Chat,
    quota: ChatQuota?,
    canManage: Boolean,
    busy: Boolean,
    openConnections: () -> Unit,
    resume: () -> Unit,
    followConversation: () -> Unit,
) {
    var options by remember(chat.id) { mutableStateOf<CodingAgentOptions?>(null) }
    var pending by remember(chat.id) { mutableStateOf<String?>(null) }
    var failure by remember(chat.id) { mutableStateOf<String?>(null) }
    var readFailure by remember(chat.id) { mutableStateOf<String?>(null) }
    var sending by remember(chat.id) { mutableStateOf(false) }
    var loading by remember(chat.id) { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val switching = pending ?: options?.switching?.provider ?: chat.codingAgentSwitch?.provider
    LaunchedEffect(chat.id, quota != null, chat.codingAgentSwitch, pending, sending) {
        if (sending || (quota == null && switching == null)) return@LaunchedEffect
        do {
            loading = options == null
            try {
                val result =
                    wireJson.decodeFromString<CodingAgentOptions>(
                        vm.api.request("GET", "/chats/${segment(chat.id)}/coding-agent")
                    )
                options = result
                if (result.switching != null && pending == null) {
                    pending = result.switching.provider
                    followConversation()
                }
                if (pending != null && result.switching == null && !sending) {
                    pending = null
                }
                readFailure = null
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (error: Exception) {
                readFailure = error.message
            } finally {
                loading = false
            }
            if (quota == null && pending == null && options?.switching == null) break
            delay(
                if (pending != null || options?.switching != null) 1000
                else if (readFailure != null) 2000 else 5000
            )
        } while (true)
    }
    val shown =
        quota
            ?: if (switching != null)
                ChatQuota(
                    codingAgent(options?.provider ?: "codex").label,
                    "",
                    false,
                    null,
                    true,
                )
            else null
    if (shown == null) return
    ChatQuotaCard(
        shown,
        canManage,
        busy || sending,
        openConnections,
        resume,
        choices = options?.choices.orEmpty(),
        loading = loading,
        switching = switching?.let { codingAgent(it).label },
        switchError = failure ?: readFailure ?: options?.switching?.error,
        switch = { provider ->
            val selected = options
            if (selected?.runId != null && selected.turnId != null && !sending) {
                sending = true
                pending = provider
                failure = null
                scope.launch {
                    try {
                        vm.api.request(
                            "POST",
                            "/chats/${segment(chat.id)}/coding-agent",
                            buildJsonObject {
                                put("requestId", UUID.randomUUID().toString())
                                put("runId", selected.runId)
                                put("turnId", selected.turnId)
                                put("provider", provider)
                            },
                        )
                        followConversation()
                    } catch (cancel: kotlinx.coroutines.CancellationException) {
                        throw cancel
                    } catch (error: Exception) {
                        pending = null
                        failure = error.message
                    } finally {
                        sending = false
                    }
                }
            }
        },
        cancelSwitch = {
            sending = true
            failure = null
            scope.launch {
                try {
                    vm.api.request("POST", "/chats/${segment(chat.id)}/stop")
                    pending = null
                    options = null
                } catch (cancel: kotlinx.coroutines.CancellationException) {
                    throw cancel
                } catch (error: Exception) {
                    failure = error.message
                } finally {
                    sending = false
                }
            }
        },
    )
}
