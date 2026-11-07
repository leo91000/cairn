package build.cairn.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

@Composable
internal fun EnvironmentExpirationNotice(state: String, error: String?) {
    Column(Modifier.padding(horizontal = 20.dp, vertical = 8.dp)) {
        Text("Environnement de travail expiré", style = MaterialTheme.typography.titleSmall)
        Text(
            if (state == "cleaning")
                "Suppression de l’environnement après inactivité. Attendez la fin du nettoyage avant d’envoyer un message."
            else
                "L’environnement a été supprimé après inactivité. Votre prochain message crée un nouvel environnement à partir de l’historique de la conversation.",
            style = MaterialTheme.typography.bodyMedium,
        )
        Text(
            "L’historique, les pièces jointes, les livrables et les liens partagés sont conservés. Les modifications non publiées et autres fichiers de travail ne sont plus disponibles.",
            style = MaterialTheme.typography.bodyMedium,
        )
        error?.let {
            Text("Le nettoyage réessaiera : $it", color = MaterialTheme.colorScheme.error)
        }
    }
}
