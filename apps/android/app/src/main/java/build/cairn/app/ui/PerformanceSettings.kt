package build.cairn.app.ui

import android.content.Intent
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import build.cairn.app.data.CairnViewModel
import build.cairn.app.diagnostics.Performance
import build.cairn.app.diagnostics.PerformanceSnapshot
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

@Composable
internal fun PerformanceSettings(vm: CairnViewModel) {
    val enabled by vm.telemetryEnabled.collectAsStateWithLifecycle(initialValue = false)
    PerformanceSettings(enabled, vm::setTelemetryEnabled)
}

@Composable
internal fun PerformanceSettings(
    telemetryEnabled: Boolean,
    setTelemetryEnabled: (Boolean) -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var snapshot by remember { mutableStateOf<PerformanceSnapshot?>(null) }
    var report by remember { mutableStateOf("") }
    suspend fun refresh() {
        val data =
            withContext(Dispatchers.Default) {
                Performance.store.snapshot() to Performance.report()
            }
        snapshot = data.first
        report = data.second
    }
    LaunchedEffect(Unit) { refresh() }
    Panel {
        Text("Diagnostic des performances", style = MaterialTheme.typography.titleLarge)
        Text(
            "Aidez à améliorer Cairn en envoyant automatiquement les mesures de performances à Beacon. Les rapports sont liés à votre compte et conservés 14 jours. Aucun message, identifiant de conversation ou secret n’est envoyé."
        )
        Toggle("Envoyer les diagnostics de performances", telemetryEnabled, setTelemetryEnabled)
        Text(
            if (telemetryEnabled) "Envoi automatique activé pendant l’utilisation de l’application."
            else "Envoi automatique désactivé. Les mesures restent sur ce téléphone."
        )
        snapshot?.let { data ->
            val frames = data.frames.sumOf { it.count }
            val missed = data.frames.sumOf { it.missedDeadline }
            val frozen = data.frames.sumOf { it.frozen }
            Text("$frames images mesurées · $missed en retard · $frozen images de 700 ms ou plus")
            Text(
                "Depuis le démarrage ou la dernière remise à zéro : ${data.elapsedMs / 1000} s.",
                style = MaterialTheme.typography.bodySmall,
            )
        }
        Text(
            "Pour analyser un ralentissement, remettez les mesures à zéro, naviguez et balayez entre les conversations, puis revenez copier le rapport."
        )
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            TextButton(onClick = { scope.launch { refresh() } }) { Text("Actualiser les mesures") }
            TextButton(
                onClick = {
                    Performance.store.clear()
                    scope.launch { refresh() }
                }
            ) {
                Text("Remettre à zéro")
            }
        }
        if (report.isNotEmpty()) {
            CopyButton("Copier le rapport de performances", report)
            OutlinedButton(
                onClick = {
                    // Build the export at the moment of sharing, including the reproduction just
                    // made.
                    scope.launch {
                        refresh()
                        val send =
                            Intent(Intent.ACTION_SEND).apply {
                                type = "text/plain"
                                putExtra(Intent.EXTRA_TEXT, report)
                            }
                        context.startActivity(Intent.createChooser(send, "Partager le diagnostic"))
                    }
                }
            ) {
                Text("Partager le rapport")
            }
        }
    }
}
