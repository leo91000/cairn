package dev.leo.manager.ui

import androidx.compose.material3.*
import androidx.compose.runtime.*
import dev.leo.manager.data.*
import kotlinx.coroutines.delay
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

@Serializable
private data class Placement(val nodes: List<ExecutionNode> = emptyList(), val pinnedNodeId: String? = null, val preferredNodeId: String? = null)

@Composable
fun NodePlacement(vm: LeoViewModel, run: Run) {
    var placement by remember(run.id) { mutableStateOf(Placement()) }
    var mode by remember(run.id) { mutableStateOf("automatic") }
    var selected by remember(run.id) { mutableStateOf(run.nodeId.orEmpty()) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(run.id) {
        try {
            placement = vm.api.get("/nodes/placement/${run.id}")
            mode = if (placement.pinnedNodeId != null) "fixed" else if (placement.preferredNodeId != null) "preferred" else "automatic"
            selected = placement.pinnedNodeId ?: placement.preferredNodeId ?: run.nodeId.orEmpty()
        } catch (e: Exception) { error = e.message }
    }
    LaunchedEffect(run.id) { while (true) { delay(1000); now = System.currentTimeMillis() } }
    run.backup?.capturedAt?.let { Text("Dernier état restaurable : ${((now-it)/1000).coerceAtLeast(0)} secondes. Le chat peut être plus récent que les fichiers sauvegardés.") }
    if (run.pinnedNodeId != null) Text("Node fixe : aucune bascule automatique ailleurs.")
    run.movementError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    Choice("Placement", mode, listOf("automatic" to "Automatique", "preferred" to "Préférer une node", "fixed" to "Fixer à une node")) { if (!busy) mode = it }
    Choice("Node", selected, placement.nodes.map { it.id to it.name }) { if (!busy) selected = it }
    fun save(move: Boolean) {
        busy = true
        error = null
        vm.perform {
            try {
                api.request("PUT", "/nodes/placement/${run.id}", buildJsonObject {
                    put("pinnedNodeId", if (mode == "fixed") JsonPrimitive(selected) else JsonNull)
                    put("preferredNodeId", if (mode == "preferred") JsonPrimitive(selected) else JsonNull)
                })
                if (move) api.request("POST", "/nodes/placement/${run.id}/move", buildJsonObject {
                    put("nodeId", selected)
                    put("cpu", run.resources?.cpu ?: 2)
                    put("memoryMiB", run.resources?.memoryMiB ?: 4096)
                    put("diskMiB", run.resources?.diskMiB ?: 32768)
                })
            } catch (e: Exception) { error = e.message } finally { busy = false }
        }
    }
    TextButton(enabled = !busy && (mode == "automatic" || selected.isNotEmpty()), onClick = { save(false) }) { Text("Enregistrer le placement") }
    TextButton(enabled = !busy && selected.isNotEmpty() && run.status in listOf("running", "succeeded") && run.sessionId != null && run.nodeState == null, onClick = { save(true) }) { Text("Déplacer maintenant") }
    Text("Une préférence autorise la reprise ailleurs. Une node fixe attend cette machine. Le déplacement suspend la conversation après réservation de la destination.")
    error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
}
