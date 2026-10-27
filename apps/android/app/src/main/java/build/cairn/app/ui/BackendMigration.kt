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
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

@Serializable private data class MigrationNode(val id: String, val name: String)

@Serializable
private data class BackendMigrationState(
    val required: Boolean = false,
    val optional: Boolean = false,
    val fromBackend: String = "firecracker",
    val toBackend: String = "firecracker",
    val nodes: List<MigrationNode> = emptyList(),
    val pending: Boolean = false,
    val reason: String? = null,
    val error: String? = null,
)

@Composable
fun BackendMigration(vm: CairnViewModel, runId: String, onCompleted: () -> Unit = {}) {
    var state by remember(runId) { mutableStateOf<BackendMigrationState?>(null) }
    var destination by remember(runId) { mutableStateOf("") }
    var confirm by remember(runId) { mutableStateOf(false) }
    var busy by remember(runId) { mutableStateOf(false) }
    var completed by remember(runId) { mutableStateOf(false) }
    var error by remember(runId) { mutableStateOf<String?>(null) }
    suspend fun load() {
        try {
            val value: BackendMigrationState =
                vm.api.get("/nodes/placement/$runId/backend-migration")
            if (
                state?.pending == true &&
                    !value.pending &&
                    value.fromBackend == state?.toBackend &&
                    value.fromBackend != state?.fromBackend
            ) {
                completed = true
                onCompleted()
            }
            state = value
            if (value.nodes.none { it.id == destination })
                destination = value.nodes.firstOrNull()?.id.orEmpty()
            error = null
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            // Old installations and non-owner views do not expose these controls.
            if (state != null) error = e.message
        }
    }
    LaunchedEffect(runId) { load() }
    LaunchedEffect(runId, state?.pending) {
        while (state?.pending == true) {
            delay(1500)
            load()
        }
    }
    val value = state ?: return
    if (!value.required && !value.optional && !value.pending && !completed) return
    val target = if (value.toBackend == "native") "l’exécution native" else "une VM Firecracker"
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        HorizontalDivider()
        Text("Changer le mode d’exécution", style = MaterialTheme.typography.titleSmall)
        when {
            completed ->
                Text(
                    "Migration terminée. Envoyez un nouveau message pour continuer dans le même fil."
                )
            value.pending ->
                Text(
                    "Migration vers $target. L’exécution est arrêtée pendant la sauvegarde et le transfert. Vous pouvez quitter cet écran."
                )
            else -> {
                Text(
                    if (value.optional)
                        "Vous pouvez choisir explicitement $target pour cette conversation. Son environnement actuel est conservé jusqu’à votre confirmation."
                    else
                        "L’agent utilise maintenant $target. Migrez cette conversation pour garder son historique et ses fichiers."
                )
                if (value.reason != null) Text(value.reason)
                else if (value.nodes.isEmpty())
                    Text(
                        "Autorisez une node de destination sur la page de l’agent avant de migrer."
                    )
                else {
                    Choice(
                        "Destination de migration",
                        destination,
                        value.nodes.map { it.id to it.name },
                    ) {
                        destination = it
                    }
                    OutlinedButton(
                        enabled = !busy && destination.isNotEmpty(),
                        onClick = { confirm = true },
                    ) {
                        Text("Migrer cette conversation…")
                    }
                }
            }
        }
        if (completed && value.optional)
            OutlinedButton(onClick = { completed = false }) {
                Text("Changer encore l’environnement")
            }
        (error ?: value.error)?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    }
    if (confirm)
        Confirm(
            title = "Migrer cette conversation ?",
            description =
                "L’exécution s’arrête et les fichiers sont transférés vers $target. Le fil et son historique restent identiques. Le prochain message ouvre une nouvelle session du moteur avec le contexte de la conversation. Certains outils ou dépendances système peuvent nécessiter une réinstallation. L’environnement d’origine est conservé si la migration échoue. La préférence de node précédente est effacée.",
            busy = busy,
            error = error,
            dismiss = { confirm = false },
        ) {
            busy = true
            vm.perform {
                try {
                    api.request(
                        "POST",
                        "/nodes/placement/$runId/backend-migration",
                        buildJsonObject {
                            put("nodeId", destination)
                            put("backend", value.toBackend)
                            put("confirm", true)
                        },
                    )
                    state = value.copy(pending = true)
                    confirm = false
                    load()
                } catch (e: Exception) {
                    error = e.message
                } finally {
                    busy = false
                }
            }
        }
}
