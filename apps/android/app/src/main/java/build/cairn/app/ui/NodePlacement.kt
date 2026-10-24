package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Info
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
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
        "pausing" to "Suspension de l’exécution",
        "saving" to "Synchronisation de l’environnement",
        "restoring" to "Restauration de l’environnement",
        "resuming" to "Reprise de la conversation",
        "waiting-for-node" to "En attente d’une node compatible",
        "updating" to "Mise à jour de la node",
    )

@Composable
fun NodePlacement(vm: CairnViewModel, run: Run) {
    val exclusive = run.suspendedByExclusive.takeIf { run.active }
    var expanded by remember(run.id) { mutableStateOf(false) }
    var moving by remember(run.id) { mutableStateOf(false) }
    var loaded by remember(run.id) { mutableStateOf(false) }
    var savedMode by remember(run.id) { mutableStateOf("automatic") }
    var savedNode by remember(run.id) { mutableStateOf("") }
    var placement by remember(run.id) { mutableStateOf(Placement()) }
    var mode by remember(run.id) { mutableStateOf("automatic") }
    var selected by remember(run.id) { mutableStateOf(run.nodeId.orEmpty()) }
    var destination by remember(run.id) { mutableStateOf("") }
    var busy by remember(run.id) { mutableStateOf(false) }
    var error by remember(run.id) { mutableStateOf<String?>(null) }
    var saved by remember(run.id) { mutableStateOf<String?>(null) }
    var migration by remember(run.id) { mutableStateOf<BackendMigrationState?>(null) }
    var conversions by remember(run.id) { mutableIntStateOf(0) }
    var privilegeError by remember(run.id) { mutableStateOf(false) }
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
    LaunchedEffect(run.id, expanded, run.executionBackend) {
        if (!expanded || run.executionBackend != "native") return@LaunchedEffect
        while (true) {
            try {
                val current = vm.api.get<Placement>("/nodes/placement/${run.id}")
                // Keep unsaved placement choices while refreshing observed privileges.
                placement = placement.copy(nodes = current.nodes)
                privilegeError = false
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (_: Exception) {
                privilegeError = true
            }
            delay(3000)
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
            exclusive != null ||
            error != null
    if (!relevant) return
    val currentNode = placement.nodes.find { it.id == run.nodeId }
    val current =
        currentNode?.name
            ?: if (run.nodeId == LOCAL_NODE_ID) "Runner du master" else "Node inconnue"
    fun request(block: suspend CairnViewModel.() -> Unit, done: String) {
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
            exclusive != null -> "Suspendue pour une réservation exclusive"
            run.nodeState != null -> nodeStates[run.nodeState] ?: run.nodeState
            run.capacityWaitUntil != null -> "En attente de capacité"
            run.backup?.status == "saving" -> "Synchronisation…"
            dirty > 0 -> "${formatBytes(dirty)} non synchronisés"
            // Native ublk disks synchronize while running like VM disks; only legacy file
            // workspaces wait for the stop.
            run.executionBackend == "native" &&
                run.status == "running" &&
                run.storage?.mode == "native" -> "Travail local · sauvegardé après l’arrêt"
            run.backup?.capturedAt != null ->
                "Synchronisé ${relativeAge(run.backup.capturedAt, now)}"
            else -> "Pas encore synchronisé"
        }
    val tone =
        when {
            failed -> MaterialTheme.colorScheme.error
            pending -> signal.warning
            exclusive != null -> signal.warning
            else -> MaterialTheme.colorScheme.onSurfaceVariant
        }
    OutlinedCard(onClick = { expanded = true }, modifier = Modifier.fillMaxWidth()) {
        ListItem(
            headlineContent = {
                Text("Node : $current", style = MaterialTheme.typography.titleSmall)
            },
            supportingContent = {
                Column {
                    Text(sync, color = tone, style = MaterialTheme.typography.bodySmall)
                }
            },
            trailingContent = { Icon(Icons.Default.Info, "Détails d’exécution") },
        )
    }
    if (expanded)
        DetailSheet("Node d’exécution", { expanded = false }) {
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(current, style = MaterialTheme.typography.titleMedium)
                if (run.executionBackend == "native")
                    Text(
                        "Dernière exécution : ${nativeModeLabel(run.nativeMode?.mode ?: "protected")} · GPU partagé"
                    )
                if (run.executionBackend == "native") {
                    val notice =
                        when {
                            privilegeError ->
                                "Impossible de confirmer les privilèges actuels de la node."
                            currentNode?.nativeModeStatus == "failed" ->
                                "Échec du basculement ; les privilèges actuels ne sont pas confirmés. Les nouveaux placements sont bloqués."
                            currentNode?.nativeModeStatus == "pending" ->
                                "Basculement en cours ; les privilèges actuels ne sont pas confirmés. Les nouveaux placements sont bloqués."
                            currentNode?.nativeModeStatus == "applied" ->
                                "Mode de node confirmé : ${nativeModeLabel(currentNode.nativeModeObserved?.mode ?: currentNode.nativeModeRequest?.mode ?: "protected")}"
                            else -> null
                        }
                    if (notice != null) Text(notice)
                }
                if (run.executionBackend == "native" && run.storage?.mode == "native")
                    Text(
                        "Sauvegarde entre les tours",
                        style = MaterialTheme.typography.bodySmall,
                    )
                // As on the web, a native run without a recorded mode uses a journal disk.
                if (
                    run.storage?.mode == "on-demand" ||
                        (run.executionBackend == "native" && run.storage?.mode != "native")
                )
                    Text(
                        "Fichiers chargés à la demande",
                        style = MaterialTheme.typography.bodySmall,
                    )
            }
            exclusive?.let { reservation ->
                Panel {
                    Text(
                        "Suspendue pour une réservation exclusive",
                        style = MaterialTheme.typography.titleSmall,
                    )
                    Text(reservation.reason)
                    Text(exclusiveReservationState(reservation.state))
                    reservation.expiresAt?.let { Text("Réservation jusqu’au ${date(it)}") }
                    Text(
                        "La reprise est automatique après la libération, dès que la node est disponible."
                    )
                }
            }
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
            BackendMigration(vm, run.id, conversions)
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
            // Once its environment exists, a conversation starts again only on nodes of its
            // backend; switching backend is an explicit move.
            val startable =
                placement.nodes.filter {
                    run.executionBackend == null || it.backend == run.executionBackend
                }
            if (mode != "automatic")
                Choice("Node", selected, startable.map { it.id to it.name }) {
                    if (!busy) selected = it
                }
            Text(
                when (mode) {
                    "preferred" ->
                        "Utilise cette node si elle est disponible, sinon reprend ailleurs."
                    "fixed" -> "Attend cette machine, sans bascule automatique."
                    else ->
                        "Choisit la node autorisée avec des slots libres et des ressources disponibles."
                } +
                    " Cela ne déplace pas la conversation maintenant." +
                    (run.executionBackend?.let {
                        " Seules les nodes « ${backendLabel(it)} » sont proposées ; utilisez Déplacer… pour changer de backend."
                    } ?: ""),
                style = MaterialTheme.typography.bodySmall,
            )
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (placement.nodes.any { it.id != run.nodeId })
                    OutlinedButton(onClick = { moving = true }, modifier = Modifier.weight(1f)) {
                        Text("Déplacer…")
                    }
                Button(
                    modifier = Modifier.weight(1f),
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
                                            if (mode == "fixed") JsonPrimitive(selected)
                                            else JsonNull,
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
                    Text("Enregistrer")
                }
            }
            saved?.let { Text(it) }
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        }
    val others = placement.nodes.filter { it.id != run.nodeId }
    val target = others.find { it.id == destination }
    // A move to the other backend keeps the same files; only a natively created disk is copied.
    val switching =
        target != null && run.executionBackend != null && target.backend != run.executionBackend
    val conversion =
        switching && target?.backend == VM_BACKEND && migration?.diskFormat == NATIVE_BACKEND
    val retryingConversion = conversion && migration?.error != null && migration?.pending == false
    LaunchedEffect(run.id, moving) {
        if (moving)
            migration =
                try {
                    vm.api.get<BackendMigrationState>(
                        "/nodes/placement/${run.id}/backend-migration"
                    )
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (_: Exception) {
                    // Without it, a natively created disk is reported by the move itself.
                    null
                }
    }
    if (moving)
        DetailSheet("Déplacer vers une autre node", { moving = false }) {
            Choice(
                "Destination",
                destination,
                others.map { it.id to "${it.name} · ${backendLabel(it.backend)}" },
            ) {
                if (!busy) destination = it
            }
            when {
                conversion ->
                    Text(
                        "Cet environnement a été créé sur une node native. Ses fichiers sont copiés une fois pour une VM ; le tour en cours s’arrête. " +
                            (if (migration?.mission == true)
                                "La mission reprend automatiquement dans une nouvelle session du moteur. "
                            else "Le prochain message ouvre une nouvelle session du moteur. ") +
                            "L’original est conservé en cas d’échec.",
                        color = MaterialTheme.colorScheme.tertiary,
                    )
                switching && target?.backend == NATIVE_BACKEND ->
                    Text(
                        "Cette conversation tournera comme processus natif de l’hôte sur ${target.name}, avec les mêmes fichiers montés sur place. Les commandes en cours s’arrêtent ; les paquets système installés dans la VM n’y sont pas disponibles. L’isolation est plus faible qu’une VM." +
                            if (!target.capabilities.vmDisks)
                                " Mettez à jour cette node pour qu’elle reçoive des conversations VM."
                            else "",
                        color = MaterialTheme.colorScheme.tertiary,
                    )
                switching ->
                    Text(
                        "Cette conversation tournera à nouveau dans une VM Firecracker, avec les mêmes fichiers. Les commandes en cours s’arrêtent ; les outils installés sur l’hôte natif ne sont pas conservés."
                    )
            }
            TextButton(
                enabled =
                    !busy &&
                        destination.isNotEmpty() &&
                        (retryingConversion ||
                            (run.status in listOf("running", "succeeded") &&
                                run.sessionId != null)) &&
                        run.nodeState == null,
                onClick = {
                    request(
                        {
                            val response =
                                api.request(
                                    "POST",
                                    "/nodes/placement/${run.id}/move",
                                    buildJsonObject {
                                        put("nodeId", destination)
                                    },
                                )
                            if (response.contains("\"migrating\"")) conversions++
                        },
                        if (switching)
                            "Déplacement demandé ; la conversation change de backend à destination."
                        else "Déplacement demandé.",
                    )
                },
            ) {
                Text(if (switching) "Déplacer et changer de backend" else "Déplacer maintenant")
            }
            Text(
                "Réserve la destination, suspend la conversation, transfère son environnement puis la reprend là-bas. Les commandes en cours sont interrompues. Les ressources sont partagées par les slots de la node.",
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
