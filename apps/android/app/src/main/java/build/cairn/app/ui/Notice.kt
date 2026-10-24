package build.cairn.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

/** What a notice tells: a state to know about, something that may need a look, or a failure. */
internal enum class NoticeTone {
    INFO,
    WARNING,
    ERROR,
}

/**
 * One way to state something beside the work: a tinted icon carries the tone, the text stays
 * readable on a quiet card. A long text is clamped and can be expanded.
 */
@Composable
internal fun Notice(
    text: String,
    modifier: Modifier = Modifier,
    tone: NoticeTone = NoticeTone.INFO,
    title: String? = null,
    icon: ImageVector =
        when (tone) {
            NoticeTone.INFO -> CairnIcons.Clock
            NoticeTone.WARNING,
            NoticeTone.ERROR -> CairnIcons.Alert
        },
    maxLines: Int = 2,
    action: (@Composable () -> Unit)? = null,
) {
    val (container, content) =
        when (tone) {
            NoticeTone.INFO ->
                MaterialTheme.colorScheme.primaryContainer to
                    MaterialTheme.colorScheme.onPrimaryContainer
            NoticeTone.WARNING -> signal.warningSoft to signal.warning
            NoticeTone.ERROR ->
                MaterialTheme.colorScheme.errorContainer to MaterialTheme.colorScheme.error
        }
    val edge =
        if (tone == NoticeTone.ERROR) MaterialTheme.colorScheme.error.copy(alpha = 0.3f)
        else MaterialTheme.colorScheme.outlineVariant
    var expanded by rememberSaveable(text) { mutableStateOf(false) }
    var clamped by remember(text) { mutableStateOf(false) }
    Surface(
        modifier.fillMaxWidth().testTag("notice-${tone.name.lowercase()}").semantics {
            liveRegion = LiveRegionMode.Polite
        },
        color = MaterialTheme.colorScheme.surface,
        contentColor = MaterialTheme.colorScheme.onSurface,
        shape = RoundedCornerShape(14.dp),
        border = BorderStroke(1.dp, edge),
    ) {
        Row(
            Modifier.padding(start = 10.dp, end = if (action != null) 4.dp else 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                Modifier.padding(vertical = 10.dp)
                    .size(28.dp)
                    .clip(RoundedCornerShape(8.dp))
                    .background(container),
                contentAlignment = Alignment.Center,
            ) {
                Icon(icon, null, Modifier.size(16.dp), tint = content)
            }
            Spacer(Modifier.width(10.dp))
            Column(
                Modifier.weight(1f)
                    .padding(vertical = 8.dp)
                    .then(
                        if (clamped || expanded)
                            Modifier.clickable(role = Role.Button) { expanded = !expanded }
                        else Modifier
                    )
            ) {
                title?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.titleSmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                if (text.isNotBlank())
                    Text(
                        text,
                        style = MaterialTheme.typography.bodySmall,
                        color =
                            if (title == null) MaterialTheme.colorScheme.onSurface
                            else MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = if (expanded) Int.MAX_VALUE else maxLines,
                        overflow = TextOverflow.Ellipsis,
                        onTextLayout = { if (!expanded) clamped = it.hasVisualOverflow },
                    )
            }
            action?.invoke()
        }
    }
}
