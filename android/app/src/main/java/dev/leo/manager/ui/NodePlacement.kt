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

private val nodeStates = mapOf("pausing" to "Suspension de la VM", "saving" to "Sauvegarde de l’environnement", "restoring" to "Restauration de l’environnement", "resuming" to "Reprise de la conversation", "waiting-for-node" to "En attente d’une node compatible", "updating" to "Mise à jour de la node")

@Composable
fun NodePlacement(vm: LeoViewModel, run: Run) {
    var placement by remember(run.id) { mutableStateOf(Placement()) }
    var mode by remember(run.id) { mutableStateOf("automatic") }
    var selected by remember(run.id) { mutableStateOf(run.nodeId.orEmpty()) }
    var destination by remember(run.id) { mutableStateOf("") }
    val initial = run.resources ?: DEFAULT_RESOURCES
    var cpu by remember(run.id) { mutableStateOf(initial.cpu.toString()) }
    var memory by remember(run.id) { mutableStateOf(gib(initial.memoryMiB)) }
    var disk by remember(run.id) { mutableStateOf(gib(initial.diskMiB)) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var saved by remember { mutableStateOf<String?>(null) }
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(run.id) {
        try {
            placement = vm.api.get("/nodes/placement/${run.id}")
            mode = if (placement.pinnedNodeId != null) "fixed" else if (placement.preferredNodeId != null) "preferred" else "automatic"
            selected = placement.pinnedNodeId ?: placement.preferredNodeId ?: run.nodeId.orEmpty()
            destination = placement.nodes.firstOrNull { it.id != run.nodeId }?.id.orEmpty()
        } catch (e: Exception) { error = e.message }
    }
    // Recovery ages are shown to the minute, so a slow clock is enough.
    LaunchedEffect(run.id) { while (true) { delay(30_000); now = System.currentTimeMillis() } }
    // With only the master runner there is nothing to choose, so stay out of the way unless something happens.
    val relevant = (run.nodeId != null && run.nodeId != LOCAL_NODE_ID) || placement.nodes.any { !it.local } ||
        run.nodeState != null || run.movementError != null || run.restoredAt != null || run.capacityWaitUntil != null || run.backup?.error != null
    if (!relevant) return
    val current = placement.nodes.find { it.id == run.nodeId }?.name ?: if (run.nodeId == LOCAL_NODE_ID) "Runner du master" else "Node inconnue"
    Text("Node : $current" + (run.resources?.let { " · ${it.cpu} CPU · ${formatMiB(it.memoryMiB)} RAM" } ?: ""))
    run.nodeState?.let { Text(nodeStates[it] ?: it) }
    run.backup?.capturedAt?.let { Text("Dernier état restaurable : ${relativeAge(it, now)}. Le chat peut être plus récent que les fichiers sauvegardés.") }
    run.backup?.error?.let { Text("Sauvegarde : $it", color = MaterialTheme.colorScheme.error) }
    run.restoredAt?.let { Text("Reprise depuis un point du ${date(it)} ; le chat plus récent reste visible.") }
    run.capacityWaitUntil?.let { Text("Attente de capacité jusqu’à ${date(it)}") }
    if (run.pinnedNodeId != null) Text("Node fixe : aucune bascule automatique ailleurs.")
    run.movementError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    fun request(block: suspend LeoViewModel.() -> Unit, done: String) {
        busy = true
        error = null
        saved = null
        vm.perform {
            try { block(); saved = done } catch (e: Exception) { error = e.message } finally { busy = false }
        }
    }
    Text("Où elle tournera la prochaine fois", style = MaterialTheme.typography.titleSmall)
    Choice("Placement", mode, listOf("automatic" to "Automatique", "preferred" to "Préférer une node", "fixed" to "Fixer à une node")) { if (!busy) mode = it }
    if (mode != "automatic") Choice("Node", selected, placement.nodes.map { it.id to it.name }) { if (!busy) selected = it }
    TextButton(enabled = !busy && (mode == "automatic" || selected.isNotEmpty()), onClick = {
        request({
            api.request("PUT", "/nodes/placement/${run.id}", buildJsonObject {
                put("pinnedNodeId", if (mode == "fixed") JsonPrimitive(selected) else JsonNull)
                put("preferredNodeId", if (mode == "preferred") JsonPrimitive(selected) else JsonNull)
            })
        }, "Préférence enregistrée. Elle s’applique au prochain démarrage ou à la prochaine reprise.")
    }) { Text("Enregistrer la préférence") }
    Text("Automatique choisit la node autorisée qui a le plus de CPU et de RAM libres. Une préférence autorise la reprise ailleurs ; une node fixe attend cette machine. Cela ne déplace pas la conversation maintenant.", style = MaterialTheme.typography.bodySmall)
    val others = placement.nodes.filter { it.id != run.nodeId }
    if (others.isNotEmpty()) {
        Text("Déplacer vers une autre node", style = MaterialTheme.typography.titleSmall)
        Choice("Destination", destination, others.map { it.id to it.name }) { if (!busy) destination = it }
        Field("CPU", cpu, { cpu = it }, enabled = !busy, keyboardOptions = InputKeyboards.Number)
        Field("RAM (Gio)", memory, { memory = it }, enabled = !busy)
        Field("Disque (Gio)", disk, { disk = it }, enabled = !busy)
        val resources = cpu.toIntOrNull()?.takeIf { it > 0 }?.let { c -> mib(memory)?.let { m -> mib(disk)?.let { d -> NodeResources(c, m, d) } } }
        TextButton(enabled = !busy && destination.isNotEmpty() && resources != null && run.status in listOf("running", "succeeded") && run.sessionId != null && run.nodeState == null, onClick = {
            request({
                api.request("POST", "/nodes/placement/${run.id}/move", buildJsonObject {
                    put("nodeId", destination)
                    put("cpu", resources!!.cpu)
                    put("memoryMiB", resources.memoryMiB)
                    put("diskMiB", resources.diskMiB)
                })
            }, "Déplacement demandé.")
        }) { Text("Déplacer maintenant") }
        Text("Réserve la destination, suspend la conversation, transfère son environnement puis la reprend là-bas. Les commandes en cours sont interrompues. Le disque ne peut pas rétrécir.", style = MaterialTheme.typography.bodySmall)
    }
    saved?.let { Text(it) }
    error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
}
