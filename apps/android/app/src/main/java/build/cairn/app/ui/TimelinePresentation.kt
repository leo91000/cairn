package build.cairn.app.ui

import build.cairn.app.data.RunEvent
import build.cairn.app.data.TimelineCard
import build.cairn.app.data.TimelineNotice
import build.cairn.contracts.TimelineContent

/*
 * Native presentation of the timeline cards the server interpreted: messages, the run cycle,
 * errors and retries. Only the wording is decided here, never what a card means.
 */

private val STATUS_TITLES =
    mapOf(
        "succeeded" to "Exécution réussie",
        "running" to "Exécution en cours",
        "failed" to "Exécution en échec",
        "queued" to "Exécution en attente",
        "cancelled" to "Exécution annulée",
        "interrupted" to "Exécution interrompue",
    )

private val FAILURE_TITLES =
    mapOf(
        "turn" to "L’agent a rencontré une erreur",
        "run" to "Exécution en échec",
        "engine" to "L’agent a rencontré une erreur",
        "runner" to "L’exécuteur a échoué",
    )

internal fun retryTitle(attempt: Long, limit: Long?): String =
    if (limit != null) "Erreur temporaire — nouvelle tentative automatique $attempt/$limit"
    else "Erreur temporaire — nouvelle tentative automatique"

internal fun retriedFailureTitle(retries: Long): String =
    if (retries == 1L) "L’agent a rencontré une erreur après 1 nouvelle tentative automatique"
    else "L’agent a rencontré une erreur après $retries nouvelles tentatives automatiques"

/** Whether a conversation shows the card now; a usage limit stops applying once it resets. */
internal fun TimelineNotice.shown(now: Long = System.currentTimeMillis()): Boolean =
    prominent && (code != "usageLimit" || usageLimit?.let { limitApplies(it, now) } != false)

internal val TimelineNotice.failed: Boolean
    get() = state == "error"

/**
 * A notice card as an activity row of the run log, or the conversation's notice. The source event
 * is kept only as the record a reader can open, never read for meaning.
 */
internal fun presentNotice(notice: TimelineNotice, source: RunEvent? = null): ActivityPresentation {
    val detail = notice.detail.orEmpty()
    val (title, subtitle) =
        when (notice.code) {
            "sessionStarted" -> "Session ouverte" to ""
            "turnStarted" -> "Travail commencé" to ""
            "turnCompleted" ->
                "Travail terminé" to
                    notice.usage
                        ?.let { "${it.inputTokens} entrée · ${it.outputTokens} sortie" }
                        .orEmpty()
            "backgroundTasks" -> {
                val tasks = notice.tasks.orEmpty().filter { it.isNotEmpty() }
                (if (tasks.isEmpty()) "Tâches en arrière-plan terminées"
                else "En attente de tâches en arrière-plan") to tasks.joinToString(" · ")
            }
            "runStatus" ->
                notice.status.orEmpty().let { status ->
                    (STATUS_TITLES[status.lowercase()]
                        ?: status.take(120).ifBlank { "État de l’exécution" }) to ""
                }
            "accountSelected" ->
                "Compte ${notice.account?.agent.orEmpty()} sélectionné" to
                    notice.account?.name.orEmpty()
            "speed" ->
                when (notice.speed) {
                    "standard" ->
                        "Ce tour passe en vitesse Standard" to
                            "Codex ne liste pas la vitesse choisie pour ce modèle ici : Cairn demande Standard"
                    "confirmed" ->
                        "Vitesse choisie demandée pour ce tour" to
                            "Codex la liste pour ce modèle ici ; l’offre du compte décide si elle s’applique"
                    else ->
                        "Vitesse non confirmée pour ce tour" to
                            "Elle n’a pas pu être confirmée pour ce compte et ce modèle ; ce tour peut passer en Standard"
                }
            "failure" ->
                when {
                    notice.retry != null -> retryTitle(notice.retry.attempt, notice.retry.limit)
                    notice.retried != null -> retriedFailureTitle(notice.retried)
                    else -> FAILURE_TITLES[notice.origin] ?: "L’agent a rencontré une erreur"
                } to ""
            "error" -> "L’agent a rencontré une erreur" to ""
            "connection" -> "Connexion interrompue" to ""
            "retryScheduled" ->
                "Nouvelle tentative programmée" to
                    notice.retry
                        ?.let {
                            "Tentative automatique ${it.attempt}${it.limit?.let { l -> "/$l" } ?: ""}"
                        }
                        .orEmpty()
            "engineRetry" ->
                notice.engineRetry.let {
                    if (it == null) "Nouvelle tentative de la requête" to ""
                    else engineRetryTitle(it) to engineRetryDetail(it)
                }
            "usageLimit" ->
                notice.usageLimit.let {
                    if (it == null) "Limite d’utilisation" to ""
                    else usageLimitTitle(it) to usageLimitDetail(it)
                }
            "engineNotice" -> engineNoticeTitle(notice.level) to ""
            "diagnostic" -> "Diagnostic de l’agent" to ""
            else -> "Résultat enregistré" to ""
        }
    val process = notice.code == "diagnostic"
    return ActivityPresentation(
        ActivityKind.NOTICE,
        title,
        subtitle = subtitle,
        state =
            when {
                notice.recovered == true -> RECOVERED_STATE
                notice.failed -> "Échec"
                notice.state == "running" -> "En cours"
                notice.state == "stopped" -> "Interrompu"
                else -> "Terminé"
            },
        failed = notice.failed,
        output = detail,
        data = source?.activityData(),
        text = source?.text ?: detail,
        verbatim = process,
    )
}

/** The text a message card shows, with Cairn's answers to questions presented natively. */
internal fun TimelineCard.messageText(): String =
    when (val content = content) {
        is TimelineContent.A -> questionAnswerText(content.text, content.questionId)
        is TimelineContent.B -> content.text
        else -> ""
    }

internal val TimelineCard.userMessage: Boolean
    get() = content is TimelineContent.A
