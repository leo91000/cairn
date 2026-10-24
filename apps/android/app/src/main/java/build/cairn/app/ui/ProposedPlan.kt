package build.cairn.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import build.cairn.app.data.RunEvent

/**
 * The approach a plan turn proposed. Only the latest one, before any later request, offers to
 * implement it: that queues an ordinary turn with the agent's usual permissions.
 */
@Composable
internal fun ProposedPlanCard(event: RunEvent, implement: (() -> Unit)?, enabled: Boolean) {
    val text = event.item().string("text").ifEmpty { event.text }
    Surface(
        Modifier.fillMaxWidth().testTag("proposed-plan"),
        shape = RoundedCornerShape(18.dp),
        color = MaterialTheme.colorScheme.surface,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column(
            Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                Icon(
                    CairnIcons.Tasks,
                    null,
                    Modifier.size(16.dp),
                    tint = MaterialTheme.colorScheme.primary,
                )
                Text(
                    "Plan proposé",
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
            Markdown(text)
            if (implement != null) {
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                Text(
                    "Rien n’a été modifié. Implémenter lance un tour avec les droits habituels de l’agent.",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Button(
                    implement,
                    Modifier.align(Alignment.End).testTag("implement-plan"),
                    enabled = enabled,
                ) {
                    Text("Implémenter")
                }
            }
        }
    }
}
