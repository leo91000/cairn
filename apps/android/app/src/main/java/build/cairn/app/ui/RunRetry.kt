package build.cairn.app.ui

import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
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
) {
    chatWaitNotice(run, retryNow(run))?.let { ChatWaitingNotice(it, openConnections, canManage) }
}

@Composable
internal fun RunWaitingReason(run: Run) {
    val message = retryNotice(run, retryNow(run)) ?: run.accountWaitReason
    message?.let {
        Notice(
            it,
            Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            title = "En attente",
            icon = CairnIcons.Hourglass,
        )
    }
}
