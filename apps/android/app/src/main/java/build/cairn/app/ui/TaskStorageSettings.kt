package build.cairn.app.ui

import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.text.input.KeyboardType
import build.cairn.app.data.CairnViewModel
import build.cairn.contracts.GetTaskStorageLimitsInput
import build.cairn.contracts.TaskStorageLimits
import build.cairn.contracts.getTaskStorageLimits
import build.cairn.contracts.saveTaskStorageLimits
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

@Composable
internal fun TaskStorageSettings(vm: CairnViewModel) {
    TaskStorageSettings(
        load = { vm.api.getTaskStorageLimits(GetTaskStorageLimitsInput()) },
        save = { vm.api.saveTaskStorageLimits(it) },
    )
}

@Composable
internal fun TaskStorageSettings(
    load: suspend () -> TaskStorageLimits,
    save: suspend (TaskStorageLimits) -> TaskStorageLimits,
) {
    var maximum by rememberSaveable { mutableStateOf("150") }
    var savedMaximum by rememberSaveable { mutableIntStateOf(150) }
    var revision by rememberSaveable { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var saved by rememberSaveable { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val parsed = maximum.toIntOrNull()
    val valid = parsed != null && parsed in 32..1024
    fun accept(policy: TaskStorageLimits) {
        maximum = policy.maxDiskGiB.toString()
        savedMaximum = policy.maxDiskGiB
        revision = policy.revision
    }
    suspend fun reload() {
        busy = true
        error = null
        try {
            accept(load())
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            error = e.message ?: "Impossible de charger la limite."
        } finally {
            busy = false
        }
    }
    LaunchedEffect(Unit) {
        if (revision.isEmpty()) reload()
    }
    Panel {
        Text("Stockage par tâche", style = MaterialTheme.typography.titleLarge)
        Text(
            "Chaque conversation ou exécution de mission démarre avec au plus 32 Gio. Les environnements natifs peuvent grandir jusqu’à cette limite commune à l’installation. L’espace de travail et les fichiers temporaires gérés sont comptés ensemble."
        )
        Text(
            "Les changements limitent les futurs agrandissements. L’espace déjà accordé est conservé ; les environnements plus grands ne peuvent plus grandir. Aucun fichier n’est supprimé."
        )
        if (revision.isNotEmpty()) {
            Field(
                "Disque maximal par tâche (Gio)",
                maximum,
                {
                    maximum = it
                    saved = false
                },
                enabled = !busy,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
            )
            Text(
                "De 32 à 1 024 Gio ; 150 Gio par défaut. Les budgets et réserves de la machine restent applicables."
            )
            Button(
                enabled = !busy && valid && parsed != savedMaximum,
                onClick = saveClick@{
                        if (busy || !valid || parsed == savedMaximum) return@saveClick
                        scope.launch {
                            busy = true
                            error = null
                            try {
                                accept(
                                    save(
                                        TaskStorageLimits(
                                            maxDiskGiB = maximum.toInt(),
                                            revision = revision,
                                        )
                                    )
                                )
                                saved = true
                            } catch (e: Exception) {
                                if (e is CancellationException) throw e
                                error = e.message ?: "Impossible d’enregistrer la limite."
                            } finally {
                                busy = false
                            }
                        }
                    },
            ) {
                Text("Enregistrer la limite")
            }
        } else if (busy) LinearProgressIndicator()
        error?.let {
            Text(it, color = MaterialTheme.colorScheme.error)
            OutlinedButton(enabled = !busy, onClick = { scope.launch { reload() } }) {
                Text("Recharger la limite enregistrée")
            }
        }
        if (saved) Text("Limite enregistrée")
    }
}
