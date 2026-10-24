package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.serialization.Serializable

/**
 * A conversation's backend and, for an environment created natively, its one-time conversion before
 * a VM can run it.
 */
@Serializable
data class BackendMigrationState(
    val backend: String = VM_BACKEND,
    val diskFormat: String = "vm",
    val pending: Boolean = false,
    val mission: Boolean = false,
    val error: String? = null,
)

/** Progress of a natively created environment converted for a VM. [refresh] restarts it. */
@Composable
fun BackendMigration(vm: CairnViewModel, runId: String, refresh: Int = 0) {
    var state by remember(runId) { mutableStateOf<BackendMigrationState?>(null) }
    var completed by remember(runId) { mutableStateOf(false) }
    suspend fun load() {
        try {
            val value: BackendMigrationState =
                vm.api.get("/nodes/placement/$runId/backend-migration")
            if (state?.pending == true && !value.pending && value.error == null) completed = true
            state = value
        } catch (e: CancellationException) {
            throw e
        } catch (_: Exception) {
            // Old installations and non-owner views have no conversion progress to show.
        }
    }
    LaunchedEffect(runId, refresh) { load() }
    LaunchedEffect(runId, state?.pending) {
        while (state?.pending == true) {
            delay(1500)
            load()
        }
    }
    val value = state ?: return
    if (!value.pending && value.error == null && !completed) return
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        HorizontalDivider()
        Text("Passage en VM", style = MaterialTheme.typography.titleSmall)
        when {
            completed ->
                Text(
                    if (value.mission)
                        "Conversion terminée. La mission reprend dans une nouvelle session du moteur."
                    else
                        "Conversion terminée. Envoyez un nouveau message pour continuer dans le même fil."
                )
            value.pending ->
                Text(
                    "Conversion de cet environnement créé en natif pour une VM. L’exécution est arrêtée pendant la copie et la restauration de ses fichiers. Vous pouvez quitter cet écran."
                )
        }
        value.error?.let {
            Text(
                "$it Déplacez à nouveau la conversation pour réessayer.",
                color = MaterialTheme.colorScheme.error,
            )
        }
    }
}
