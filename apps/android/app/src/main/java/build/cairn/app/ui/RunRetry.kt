package build.cairn.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import build.cairn.app.data.Run
import kotlin.math.ceil

/** Mirrors the web retry notice; server admission remains authoritative at expiry. */
internal fun retryNotice(run: Run?, now: Long = System.currentTimeMillis()): String? {
    val retry = run?.retry ?: return null
    val nextAttempt = retry.nextAttemptAt?.takeIf { it != 0L } ?: return null
    if (run.status != "queued" || !run.accountRequired.isNullOrEmpty()) return null

    val reason = run.accountWaitReason?.trim().orEmpty()
    if (reason.isNotEmpty() && !reason.startsWith("Temporary agent error.")) return null

    val seconds = ceil((nextAttempt.toDouble() - now.toDouble()) / 1000).toLong().coerceAtLeast(0)
    val whenRetry = if (seconds > 0) "in $seconds seconds" else "now"
    return "Temporary agent error. Retrying $whenRetry (attempt ${retry.attempt}/${retry.limit})."
}

/** Tick only a visible retry, inside its notice's composition; never request server updates. */
@Composable
private fun retryNow(run: Run?): Long =
    if (retryNotice(run) != null) rememberNow(run?.retry?.nextAttemptAt)
    else System.currentTimeMillis()

@Composable
internal fun ChatRunWaitingNotice(
    run: Run?,
    openConnections: () -> Unit,
    canManage: Boolean = true,
    busy: Boolean = false,
    cancelRetry: (() -> Unit)? = null,
) {
    val now = retryNow(run)
    if (run != null && retryNotice(run, now) != null) {
        RecoveryNotice(run, now, busy, cancelRetry)
        return
    }
    chatWaitNotice(run, now)?.let { ChatWaitingNotice(it, openConnections, canManage) }
}

@Composable
internal fun RunWaitingReason(run: Run, busy: Boolean = false, cancelRetry: (() -> Unit)? = null) {
    val now = retryNow(run)
    if (retryNotice(run, now) != null) {
        RecoveryNotice(run, now, busy, cancelRetry)
        return
    }
    run.accountWaitReason?.let {
        Notice(
            it,
            Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            title = "En attente",
            icon = CairnIcons.Hourglass,
        )
    }
}

/** One local clock, an attempt budget and a real pause/stop action. */
@Composable
internal fun RecoveryNotice(run: Run, now: Long, busy: Boolean, cancelRetry: (() -> Unit)?) {
    val retry = run.retry ?: return
    val seconds =
        ceil(((retry.nextAttemptAt ?: now) - now).toDouble() / 1000).toLong().coerceAtLeast(0)
    val countdown =
        if (seconds >= 60) "${seconds / 60}:${(seconds % 60).toString().padStart(2, '0')}"
        else "${seconds}s"
    var details by rememberSaveable(run.id) { mutableStateOf(false) }
    val accent = MaterialTheme.colorScheme.primary
    Surface(
        Modifier.fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp)
            .testTag("recovery-notice"),
        color = MaterialTheme.colorScheme.surface,
        shape = RoundedCornerShape(16.dp),
        border = BorderStroke(1.dp, accent.copy(alpha = 0.28f)),
    ) {
        Column {
            Column(Modifier.fillMaxWidth().background(accent.copy(alpha = 0.06f)).padding(14.dp)) {
                Row(
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                    verticalAlignment = Alignment.Top,
                ) {
                    Surface(color = accent.copy(alpha = 0.12f), shape = RoundedCornerShape(10.dp)) {
                        Icon(
                            CairnIcons.Clock,
                            null,
                            Modifier.padding(9.dp).size(18.dp),
                            tint = accent,
                        )
                    }
                    Column(Modifier.weight(1f)) {
                        Text(
                            if (retry.cause.startup) "Reconnexion automatique"
                            else "Nouvelle tentative automatique",
                            Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                            style = MaterialTheme.typography.titleSmall,
                        )
                        Text(
                            "Votre conversation et vos fichiers sont conservés.",
                            Modifier.padding(top = 4.dp),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                Row(
                    Modifier.fillMaxWidth().padding(top = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    Text(
                        if (seconds > 0) "Reprise dans" else "Préparation de la reprise",
                        Modifier.weight(1f),
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Text(
                        if (seconds > 0) countdown else "Bientôt",
                        color = accent,
                        style = MaterialTheme.typography.titleLarge,
                        fontWeight = FontWeight.SemiBold,
                    )
                }
            }
            Column(Modifier.padding(start = 14.dp, end = 14.dp, top = 12.dp, bottom = 4.dp)) {
                Row(
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Row(
                        Modifier.weight(1f).semantics {
                            progressBarRangeInfo =
                                ProgressBarRangeInfo(
                                    retry.attempt.toFloat(),
                                    0f..retry.limit.toFloat(),
                                    retry.limit,
                                )
                            contentDescription = "Reprises automatiques"
                        },
                        horizontalArrangement = Arrangement.spacedBy(5.dp),
                    ) {
                        repeat(retry.limit) { index ->
                            val color =
                                when {
                                    index + 1 == retry.attempt -> accent
                                    index + 1 < retry.attempt -> accent.copy(alpha = 0.4f)
                                    else -> MaterialTheme.colorScheme.surfaceContainerHighest
                                }
                            Surface(
                                Modifier.weight(1f).height(5.dp),
                                color = color,
                                shape = RoundedCornerShape(5.dp),
                            ) {}
                        }
                    }
                    Text(
                        "Reprise ${retry.attempt}/${retry.limit}",
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                FlowRow(
                    Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    TextButton(onClick = { details = !details }) {
                        Text(if (details) "Masquer le détail" else "Voir le détail")
                    }
                    if (cancelRetry != null)
                        TextButton(onClick = cancelRetry, enabled = !busy) {
                            Text("Annuler la reprise")
                        }
                }
                if (details)
                    Text(
                        retry.cause.message,
                        Modifier.padding(bottom = 10.dp),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 6,
                    )
            }
        }
    }
}
