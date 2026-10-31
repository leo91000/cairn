package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import build.cairn.app.data.TimelineNotice

/** Whether a reason repeats one already given; a run's recorded error may be truncated. */
internal fun sameReason(given: String, reason: String) =
    given == reason || (reason.isNotEmpty() && given.startsWith(reason))

/** A notice the server marked for the conversation: a failure, retry, reconnect or warning. */
@Composable
internal fun ChatNotice(notice: TimelineNotice) {
    val presentation = presentNotice(notice)
    // A failure Cairn retries, or a reconnect, is a warning rather than a dead end.
    val failed = notice.failed && notice.code != "connection" && notice.retry == null
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
            Text(presentation.title, style = MaterialTheme.typography.labelLarge)
            val detail = presentation.output.ifBlank { presentation.subtitle }
            if (detail.isNotBlank() && detail != presentation.title)
                Text(detail, style = MaterialTheme.typography.bodySmall)
        }
    }
}
