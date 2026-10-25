package build.cairn.app.ui

import build.cairn.app.data.RunEvent
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.longOrNull

/*
 * What the coding engine reports about its own requests: retries it makes itself, the provider's
 * usage limit, and its notices. None of them is a failure of the turn, nor a wait Cairn manages.
 */

internal val ENGINE_EVENTS = setOf("engine.retry", "engine.notice", "provider.limit")

/** Marks, for the run log, an engine retry that work continued after. */
internal val RECOVERED = "recovered" to JsonPrimitive(true)

private val RETRY_REASONS =
    mapOf(
        "rate_limit" to "Limite de débit",
        "overloaded" to "Serveurs surchargés",
        "server_error" to "Erreur serveur",
        "authentication_failed" to "Échec d’authentification",
        "billing_error" to "Erreur de facturation",
        "invalid_request" to "Requête invalide",
        "model_not_found" to "Modèle introuvable",
        "max_output_tokens" to "Limite de sortie atteinte",
    )

private val LIMIT_WINDOWS =
    mapOf(
        "five_hour" to "Limite de 5 heures",
        "seven_day" to "Limite hebdomadaire",
        "seven_day_opus" to "Limite hebdomadaire Opus",
        "seven_day_sonnet" to "Limite hebdomadaire Sonnet",
        "seven_day_overage_included" to "Limite hebdomadaire",
        "overage" to "Limite d’utilisation supplémentaire",
    )

private fun JsonObject?.count(key: String): Long? =
    (this?.get(key) as? JsonPrimitive)?.longOrNull?.takeIf { it >= 0 }

private fun JsonObject?.flag(key: String): Boolean =
    (this?.get(key) as? JsonPrimitive)?.booleanOrNull == true

private fun duration(ms: Long): String =
    when {
        ms < 1000 -> "moins d’une seconde"
        ms < 90_000 -> "${(ms + 500) / 1000} s"
        else -> "${(ms + 30_000) / 60_000} min"
    }

internal fun engineRetryTitle(data: JsonObject?): String {
    val attempt = data.count("attempt")
    val limit = data.count("maxRetries")?.takeIf { it > 0 }
    val progress =
        when {
            attempt == null -> ""
            limit != null -> " · tentative $attempt/$limit"
            else -> " · tentative $attempt"
        }
    return "Claude Code relance la requête$progress"
}

internal fun engineRetryDetail(data: JsonObject?): String {
    val noResponse = data?.get("noResponse") is JsonObject
    val status = data.count("status")?.takeIf { it > 0 }
    val delay = data.count("delayMs")
    return listOfNotNull(
            if (noResponse) "Aucune réponse de l’API" else RETRY_REASONS[data.string("reason")],
            status?.let { "HTTP $it" },
            delay?.let { "prochaine tentative dans ${duration(it)}" },
        )
        .joinToString(" · ")
}

/** When a limit resets, in the reader's time, with the day once it is not today. */
internal fun limitReset(
    resetsAt: Long?,
    zone: ZoneId = ZoneId.systemDefault(),
    today: LocalDate = LocalDate.now(zone),
): String {
    if (resetsAt == null || resetsAt <= 0) return ""
    val moment = Instant.ofEpochMilli(resetsAt).atZone(zone)
    val time = moment.format(DateTimeFormatter.ofPattern("HH:mm", Locale.FRENCH))
    return if (moment.toLocalDate() == today) time
    else "${moment.format(DateTimeFormatter.ofPattern("EEE d MMM", Locale.FRENCH))} $time"
}

internal fun usageLimitTitle(data: JsonObject?): String {
    val window = LIMIT_WINDOWS[data.string("window")] ?: "Limite d’utilisation"
    return when {
        data.string("status") == "allowed" -> "La limite d’utilisation ne s’applique plus"
        data.string("status") == "allowed_warning" -> "$window bientôt atteinte"
        !data.flag("blocked") -> "$window atteinte · utilisation supplémentaire en cours"
        else -> "$window atteinte"
    }
}

internal fun usageLimitDetail(data: JsonObject?): String =
    limitReset(data.count("resetsAt")).let { if (it.isEmpty()) "" else "Réinitialisation à $it" }

internal fun engineNoticeTitle(data: JsonObject?): String =
    when (data.string("level")) {
        "warning" -> "Avertissement de Claude Code"
        "suggestion" -> "Suggestion de Claude Code"
        else -> "Note de Claude Code"
    }

/** A limit no longer applies once its window resets. */
private fun limitApplies(data: JsonObject?, now: Long): Boolean =
    data.string("status") != "allowed" && (data.count("resetsAt") ?: Long.MAX_VALUE) > now

/**
 * Whether a conversation shows the event as a notice. Informational notices stay in the run log.
 */
internal fun engineAttention(
    type: String,
    data: JsonObject?,
    now: Long = System.currentTimeMillis(),
): Boolean =
    when (type) {
        "engine.retry" -> true
        "provider.limit" -> limitApplies(data, now)
        "engine.notice" ->
            data.string("level") in listOf("warning", "suggestion") || data.flag("stopsTurn")
        else -> false
    }

/**
 * Whether the agent's own work goes on: its reply streams or its tools start, a subagent among
 * them. Claude Code reports a request it gave up on as a completed message, which the turn's
 * failure follows; a subagent's progress, or its resumption, says nothing about the agent's
 * request.
 */
private fun workContinues(event: RunEvent): Boolean {
    if (event.type == "turn.completed") return true
    val item = event.item()
    if (item.string("parent_id").isNotEmpty()) return false
    val subagent = item.string("type") == "subagent"
    return when (event.type) {
        "item.started" -> !subagent || !item.flag("resumed")
        "item.updated" -> !subagent
        else -> false
    }
}

/** A run that ends without its turn completing, stopped or failed by the runner. */
private fun runEnds(event: RunEvent): Boolean =
    event.type == "runner.failed" || (event.type == "status" && event.text in TERMINAL_STATUSES)

/** The engine's latest retry before work continued, which the run log marks recovered. */
internal fun recoveredEngineRetries(events: List<RunEvent>): Set<Long> {
    val recovered = mutableSetOf<Long>()
    var retry: Long? = null
    for (event in events) {
        when {
            event.type == "turn.started" || event.type == "turn.failed" || runEnds(event) ->
                retry = null
            workContinues(event) -> {
                retry?.let(recovered::add)
                retry = null
            }
            event.type == "engine.retry" -> retry = event.id
        }
    }
    return recovered
}

/**
 * Engine notices a conversation no longer shows: the engine's retries share one notice until work
 * continues, the usage limit shows its latest state until it no longer applies, and the failure
 * that ends a turn says why it ended (a limit that stopped it shows as the usage banner).
 */
internal fun hiddenEngineNotices(
    events: List<RunEvent>,
    now: Long = System.currentTimeMillis(),
): Set<Long> {
    val hidden = mutableSetOf<Long>()
    val retries = mutableListOf<Long>()
    val limits = mutableListOf<Long>()
    for (event in events) {
        when {
            event.type == "turn.started" -> {
                retries.clear()
                limits.clear()
            }
            event.type == "turn.failed" -> {
                hidden.addAll(retries + limits)
                retries.clear()
                limits.clear()
            }
            // The engine's retries and usage limit belong to the run that reported them: its
            // retries did not recover, and a later run reports its own limit.
            runEnds(event) -> {
                hidden.addAll(retries + limits)
                retries.clear()
                limits.clear()
            }
            workContinues(event) -> {
                hidden.addAll(retries)
                retries.clear()
            }
            event.type == "engine.retry" -> {
                hidden.addAll(retries)
                retries.clear()
                retries.add(event.id)
            }
            event.type == "provider.limit" -> {
                hidden.addAll(limits)
                limits.clear()
                if (limitApplies(event.activityData(), now)) limits.add(event.id)
                else hidden.add(event.id)
            }
        }
    }
    return hidden
}
