package dev.leo.manager.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Info
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import dev.leo.manager.data.*
import kotlinx.coroutines.delay
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

@Serializable
private data class Placement(
    val nodes: List<ExecutionNode> = emptyList(),
    val pinnedNodeId: String? = null,
    val preferredNodeId: String? = null,
)

private val nodeStates =
    mapOf(
        "pausing" to "Suspension de la VM",
        "saving" to "Synchronisation de l’environnement",
        "restoring" to "Restauration de l’environnement",
        "resuming" to "Reprise de la conversation",
        "waiting-for-node" to "En attente d’une node compatible",
        "updating" to "Mise à jour de la node",
    )

@Composable
fun NodePlacement(vm: LeoViewModel, run: Run) {
    var expanded by remember(run.id) { mutableStateOf(false) }
    var moving by remember(run.id) { mutableStateOf(false) }
    var loaded by remember(run.id) { mutableStateOf(false) }
    var savedMode by remember(run.id) { mutableStateOf("automatic") }
    var savedNode by remember(run.id) { mutableStateOf("") }
    var placement by remember(run.id) { mutableStateOf(Placement()) }
    var mode by remember(run.id) { mutableStateOf("automatic") }
    var selected by remember(run.id) { mutableStateOf(run.nodeId.orEmpty()) }
    var destination by remember(run.id) { mutableStateOf("") }
    val initial = run.resources ?: DEFAULT_RESOURCES
    var cpu by remember(run.id) { mutableStateOf(initial.cpu.toString()) }
    var memory by remember(run.id) { mutableStateOf(gib(initial.memoryMiB)) }
    var disk by remember(run.id) { mutableStateOf(gib(initial.diskMiB)) }
    var busy by remember(run.id) { mutableStateOf(false) }
    var error by remember(run.id) { mutableStateOf<String?>(null) }
    var saved by remember(run.id) { mutableStateOf<String?>(null) }
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(run.id) {
        try {
            placement = vm.api.get("/nodes/placement/${run.id}")
            mode =
                if (placement.pinnedNodeId != null) "fixed"
                else if (placement.preferredNodeId != null) "preferred" else "automatic"
            selected = placement.pinnedNodeId ?: placement.preferredNodeId ?: run.nodeId.orEmpty()
            savedMode = mode
            savedNode = selected
            loaded = true
            destination = placement.nodes.firstOrNull { it.id != run.nodeId }?.id.orEmpty()
        } catch (e: Exception) {
            error = e.message
        }
    }
    // Recovery ages are shown to the minute, so a slow clock is enough.
    LaunchedEffect(run.id) {
        while (true) {
            delay(30_000)
            now = System.currentTimeMillis()
        }
    }
    // With only the master runner there is nothing to choose, so stay out of the way unless
    // something happens.
    val relevant =
        (run.nodeId != null && run.nodeId != LOCAL_NODE_ID) ||
            placement.nodes.any { !it.local } ||
            run.nodeState != null ||
            run.movementError != null ||
            run.restoredAt != null ||
            run.capacityWaitUntil != null ||
            run.backup?.error != null ||
            run.storage?.mode == "on-demand" ||
            error != null
    if (!relevant) return
    val current =
        placement.nodes.find { it.id == run.nodeId }?.name
            ?: if (run.nodeId == LOCAL_NODE_ID) "Runner du master" else "Node inconnue"
    fun request(block: suspend LeoViewModel.() -> Unit, done: String) {
        busy = true
        error = null
        saved = null
        vm.perform {
            try {
                block()
                saved = done
            } catch (e: Exception) {
                error = e.message
            } finally {
                busy = false
            }
        }
    }
    val dirty = run.storage?.dirtyBytes ?: 0L
    val failed =
        run.backup?.error != null ||
            run.movementError != null ||
            run.storage?.waitingFor == "integrity"
    val pending = dirty > 0 || run.backup?.status == "saving" || run.storage?.waitingFor != null
    val sync =
        when {
            run.movementError != null -> "Déplacement en échec"
            run.backup?.error != null -> "Synchronisation en échec"
            run.storage?.waitingFor != null -> storageState(run.storage.waitingFor)
            run.nodeState != null -> nodeStates[run.nodeState] ?: run.nodeState
            run.capacityWaitUntil != null -> "En attente de capacité"
            run.backup?.status == "saving" -> "Synchronisation…"
            dirty > 0 -> "${formatBytes(dirty)} non synchronisés"
            run.backup?.capturedAt != null ->
                "Synchronisé ${relativeAge(run.backup.capturedAt, now)}"
            else -> "Pas encore synchronisé"
        }
    val tone =
        when {
            failed -> MaterialTheme.colorScheme.error
            pending -> signal.warning
            else -> MaterialTheme.colorScheme.onSurfaceVariant
        }
    OutlinedCard(onClick = { expanded = true }, modifier = Modifier.fillMaxWidth()) {
        ListItem(
            headlineContent = {
                Text("Node : $current", style = MaterialTheme.typography.titleSmall)
            },
            supportingContent = {
                Column {
                    run.resources?.let { Text("${it.cpu} CPU · ${formatMiB(it.memoryMiB)} RAM") }
                    Text(sync, color = tone, style = MaterialTheme.typography.bodySmall)
                }
            },
            trailingContent = { Icon(Icons.Default.Info, "Détails d’exécution") },
        )
    }
    if (expanded)
        DetailSheet("Node d’exécution", { expanded = false }) {
            Text(current, style = MaterialTheme.typography.titleMedium)
            run.resources?.let {
                Text(
                    "${it.cpu} CPU · ${formatMiB(it.memoryMiB)} RAM",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            if (run.storage?.mode == "on-demand")
                Text("Fichiers chargés à la demande", style = MaterialTheme.typography.bodySmall)
            Panel {
                Text("Synchronisation", style = MaterialTheme.typography.titleSmall)
                Text(sync, color = tone)
                run.storage?.localBytes?.let { local ->
                    LinearProgressIndicator(
                        progress = {
                            if (local > 0)
                                ((local - dirty).coerceAtLeast(0).toFloat() / local).coerceIn(
                                    0f,
                                    1f,
                                )
                            else 0f
                        },
                        modifier = Modifier.fillMaxWidth(),
                        trackColor =
                            if (dirty > 0) tone else MaterialTheme.colorScheme.surfaceVariant,
                    )
                    Text(
                        "${formatBytes(local)} sur cette node · ${formatBytes(dirty)} non synchronisés",
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                if (dirty > 0)
                    Text(
                        "Ces changements existent uniquement sur cette node" +
                            (run.storage?.dirtySince?.let { " (${relativeAge(it, now)})" } ?: "") +
                            ".",
                        style = MaterialTheme.typography.bodySmall,
                    )
                run.backup?.capturedAt?.let {
                    Text(
                        "Dernière synchronisation : ${relativeAge(it, now)}",
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                run.backup?.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                run.movementError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                run.restoredAt?.let {
                    Text(
                        "Reprise depuis un point du ${date(it)} ; le chat plus récent reste visible."
                    )
                }
                run.capacityWaitUntil?.let { Text("Attente de capacité jusqu’à ${date(it)}") }
            }
            if (savedMode == "fixed")
                Text(
                    "Node fixe : aucune bascule automatique ailleurs.",
                    style = MaterialTheme.typography.bodySmall,
                )
            Text("Où elle tournera la prochaine fois", style = MaterialTheme.typography.titleSmall)
            SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
                listOf("automatic" to "Automatique", "preferred" to "Préférer", "fixed" to "Fixer")
                    .forEachIndexed { index, (value, label) ->
                        SegmentedButton(
                            selected = mode == value,
                            onClick = { mode = value },
                            enabled = loaded && !busy,
                            shape = SegmentedButtonDefaults.itemShape(index, 3),
                        ) {
                            Text(label)
                        }
                    }
            }
            if (mode != "automatic")
                Choice("Node", selected, placement.nodes.map { it.id to it.name }) {
                    if (!busy) selected = it
                }
            TextButton(
                enabled =
                    loaded &&
                        !busy &&
                        (mode != savedMode || (mode != "automatic" && selected != savedNode)) &&
                        (mode == "automatic" || selected.isNotEmpty()),
                onClick = {
                    request(
                        {
                            val requestedMode = mode
                            val requestedNode = selected
                            api.request(
                                "PUT",
                                "/nodes/placement/${run.id}",
                                buildJsonObject {
                                    put(
                                        "pinnedNodeId",
                                        if (mode == "fixed") JsonPrimitive(selected) else JsonNull,
                                    )
                                    put(
                                        "preferredNodeId",
                                        if (mode == "preferred") JsonPrimitive(selected)
                                        else JsonNull,
                                    )
                                },
                            )
                            savedMode = requestedMode
                            savedNode = requestedNode
                        },
                        "Préférence enregistrée. Elle s’applique au prochain démarrage ou à la prochaine reprise.",
                    )
                },
            ) {
                Text("Enregistrer la préférence")
            }
            Text(
                when (mode) {
                    "preferred" ->
                        "Utilise cette node si elle est disponible, sinon reprend ailleurs."
                    "fixed" -> "Attend cette machine, sans bascule automatique."
                    else -> "Choisit la node autorisée avec le plus de CPU et de RAM libres."
                } + " Cela ne déplace pas la conversation maintenant.",
                style = MaterialTheme.typography.bodySmall,
            )
            val others = placement.nodes.filter { it.id != run.nodeId }
            if (others.isNotEmpty())
                OutlinedButton(onClick = { moving = true }) { Text("Déplacer…") }
            saved?.let { Text(it) }
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        }
    val others = placement.nodes.filter { it.id != run.nodeId }
    if (moving)
        DetailSheet("Déplacer vers une autre node", { moving = false }) {
            Choice("Destination", destination, others.map { it.id to it.name }) {
                if (!busy) destination = it
            }
            Field(
                "CPU",
                cpu,
                { cpu = it },
                enabled = !busy,
                keyboardOptions = InputKeyboards.Number,
            )
            Field("RAM (Gio)", memory, { memory = it }, enabled = !busy)
            Field("Disque (Gio)", disk, { disk = it }, enabled = !busy)
            val resources =
                cpu.toIntOrNull()
                    ?.takeIf { it > 0 }
                    ?.let { c ->
                        mib(memory)?.let { m -> mib(disk)?.let { d -> NodeResources(c, m, d) } }
                    }
            TextButton(
                enabled =
                    !busy &&
                        destination.isNotEmpty() &&
                        resources != null &&
                        resources.memoryMiB >= 128 &&
                        resources.diskMiB >= initial.diskMiB &&
                        run.status in listOf("running", "succeeded") &&
                        run.sessionId != null &&
                        run.nodeState == null,
                onClick = {
                    request(
                        {
                            api.request(
                                "POST",
                                "/nodes/placement/${run.id}/move",
                                buildJsonObject {
                                    put("nodeId", destination)
                                    put("cpu", resources!!.cpu)
                                    put("memoryMiB", resources.memoryMiB)
                                    put("diskMiB", resources.diskMiB)
                                },
                            )
                        },
                        "Déplacement demandé.",
                    )
                },
            ) {
                Text("Déplacer maintenant")
            }
            Text(
                "Réserve la destination, suspend la conversation, transfère son environnement puis la reprend là-bas. Les commandes en cours sont interrompues. Le disque ne peut pas rétrécir.",
                style = MaterialTheme.typography.bodySmall,
            )
            saved?.let { Text(it) }
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        }
}

private fun storageState(state: String): String =
    when (state) {
        "storage-unavailable" -> "En attente du stockage"
        "disk-space" -> "En pause : espace disque insuffisant"
        "backup-lag" -> "En pause pendant la synchronisation"
        "integrity" -> "Le disque nécessite une vérification"
        else -> state
    }
