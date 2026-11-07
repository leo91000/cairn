package build.cairn.app.ui

import build.cairn.contracts.EngineRetry
import build.cairn.contracts.UsageLimit
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

/*
 * How the conversation words what the coding engine reports about its own requests: retries it
 * makes itself, the provider's usage limit, and its notices. The server interprets them; these
 * only localize its codes and arguments.
 */

/** The run log's state for a recovered engine retry. */
internal const val RECOVERED_STATE = "Rétablie"

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

private fun duration(ms: Long): String =
    when {
        ms < 1000 -> "moins d’une seconde"
        ms < 90_000 -> "${(ms + 500) / 1000} s"
        else -> "${(ms + 30_000) / 60_000} min"
    }

internal fun engineRetryTitle(retry: EngineRetry): String {
    val progress =
        when {
            retry.attempt == null -> ""
            retry.maxRetries != null -> " · tentative ${retry.attempt}/${retry.maxRetries}"
            else -> " · tentative ${retry.attempt}"
        }
    return "Claude Code relance la requête$progress"
}

internal fun engineRetryDetail(retry: EngineRetry): String =
    listOfNotNull(
            if (retry.noResponse) "Aucune réponse de l’API" else RETRY_REASONS[retry.reason],
            retry.httpStatus?.let { "HTTP $it" },
            retry.delayMs?.let { "prochaine tentative dans ${duration(it)}" },
        )
        .joinToString(" · ")

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

internal fun usageLimitTitle(limit: UsageLimit): String {
    val window = LIMIT_WINDOWS[limit.window] ?: "Limite d’utilisation"
    return when {
        limit.status == "allowed" -> "La limite d’utilisation ne s’applique plus"
        limit.status == "allowed_warning" -> "$window bientôt atteinte"
        !limit.blocked -> "$window atteinte · utilisation supplémentaire en cours"
        else -> "$window atteinte"
    }
}

internal fun usageLimitDetail(limit: UsageLimit): String =
    limitReset(limit.resetsAt).let { if (it.isEmpty()) "" else "Réinitialisation à $it" }

/** A limit no longer applies once its window resets. */
internal fun limitApplies(limit: UsageLimit, now: Long = System.currentTimeMillis()): Boolean =
    limit.status != "allowed" && (limit.resetsAt ?: Long.MAX_VALUE) > now

internal fun engineNoticeTitle(level: String?): String =
    when (level) {
        "warning" -> "Avertissement de Claude Code"
        "suggestion" -> "Suggestion de Claude Code"
        else -> "Note de Claude Code"
    }
