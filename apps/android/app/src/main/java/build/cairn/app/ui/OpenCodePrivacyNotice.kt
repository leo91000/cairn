package build.cairn.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

internal fun isOpenCodePrivacyError(message: String) =
    message.startsWith("OpenCode Go requires Global regions for this model.")

@Composable
internal fun OpenCodePrivacyNotice(
    enabled: Boolean,
    retry: (() -> Unit)?,
    create: (() -> Unit)?,
    dismiss: () -> Unit,
) {
    Surface(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.surfaceContainerLow,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(verticalAlignment = Alignment.Top) {
                Text(
                    "Ce modèle nécessite un réglage Privacy",
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.titleSmall,
                )
                ActionIcon("Masquer l’erreur", CairnIcons.Close, onClick = dismiss)
            }
            Text(
                "OpenCode Go exige les régions Global pour ce modèle. Vérifiez les réglages Privacy de votre espace OpenCode avant de réessayer.",
                style = MaterialTheme.typography.bodySmall,
            )
            ExternalButton("Réglages OpenCode", "https://opencode.ai/console/")
            retry?.let {
                OutlinedButton(onClick = it, enabled = enabled) { Text("Réessayer la réponse") }
            }
            // Only someone who may use this agent is offered to start again with it.
            create?.let {
                Text(
                    "Pour garder vos réglages Privacy, choisissez un autre modèle dans une nouvelle conversation. Celle-ci et ses fichiers restent conservés.",
                    style = MaterialTheme.typography.bodySmall,
                )
                TextButton(onClick = it, enabled = enabled) { Text("Nouvelle conversation") }
            }
        }
    }
}
