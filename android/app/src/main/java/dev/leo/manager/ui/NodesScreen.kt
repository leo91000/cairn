package dev.leo.manager.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import dev.leo.manager.data.*
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.put
import java.util.Date

@Composable
fun NodesScreen(vm: LeoViewModel, state: Workspace) {
    var nodes by remember { mutableStateOf<List<ExecutionNode>>(emptyList()) }
    var name by remember { mutableStateOf("") }
    // Never persist enrollment secrets in saved instance state.
    var enrollment by remember { mutableStateOf<NodeEnrollment?>(null) }
    var recovery by remember { mutableStateOf<NodeBackupSettings?>(null) }
    var editing by remember { mutableStateOf<ExecutionNode?>(null) }
    var revoking by remember { mutableStateOf<ExecutionNode?>(null) }
    suspend fun load() { nodes = vm.api.get("/nodes") }
    Poll("nodes", 10_000) {
        try { load() } catch (e: Exception) { vm.report(e) }
    }
    Page {
        Heading("Nodes", "Vos machines de confiance. Le GPU est différé.")
        Text("Les conversations CPU utilisent Firecracker. Une node doit être connectée avec un runtime compatible et des ressources disponibles.")
        Panel {
            Field("Nom de la machine", name, { name = it })
            Button(enabled = !state.busy && name.isNotBlank(), onClick = {
                vm.perform {
                    enrollment = api.send("POST", "/nodes/enrollments", buildJsonObject { put("name", name) })
                }
            }) { Text("Créer un code d’inscription") }
            enrollment?.let {
                Text("Code à usage unique · expire le ${Date(it.expiresAt)}")
                Code(it.code)
                CopyButton("Copier le code", it.code)
                it.installCommand?.let { command ->
                    Text("Sur la node Linux de confiance, lancez cette commande puis saisissez le code :")
                    Code(command)
                    CopyButton("Copier la commande", command)
                }
                TextButton(onClick = { enrollment = null }) { Text("Masquer") }
            }
        }
        TextButton(onClick = { vm.perform { recovery = api.get("/nodes/settings") } }) { Text("Configurer les sauvegardes") }
        nodes.forEach { node ->
            Panel {
                Text(node.name, style = MaterialTheme.typography.titleLarge)
                Text(when (node.status) { "online" -> "Connectée"; "offline" -> "Déconnectée"; "revoked" -> "Révoquée"; "local" -> "Runner local"; else -> node.status })
                Text("${node.capabilities.os} · ${node.capabilities.arch} · KVM ${if (node.capabilities.kvm) "disponible" else "indisponible"}")
                Text("Détecté : ${node.capabilities.cpu} CPU · ${node.capabilities.memoryMiB} Mio RAM · ${node.capabilities.diskMiB} Mio disque")
                if (node.runtimeId.isNotBlank()) Text("Runtime : ${node.runtimeId}")
                Text("${node.limits.cpu} CPU · ${node.limits.memoryMiB} Mio RAM · ${node.limits.diskMiB} Mio disque autorisés")
                node.reserved?.let { Text("Réservé : ${it.cpu} CPU · ${it.memoryMiB} Mio RAM · ${it.diskMiB} Mio disque") }
                node.available?.let { Text("Disponible : ${it.cpu} CPU · ${it.memoryMiB} Mio RAM · ${it.diskMiB} Mio disque") }
                node.maintenance?.let { Text(if (it == "draining") "Mise en pause et sauvegarde des conversations" else "Prête à redémarrer pour la mise à jour")
                    node.maintenanceError?.let { message -> Text(message, color = MaterialTheme.colorScheme.error) }
                }
                if (node.systemTags.isNotEmpty()) Text("Tags détectés : ${node.systemTags.joinToString(" · ")}")
                node.imageDigest?.let { Text("Version : $it") }
                node.updateError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                if (node.tags.isNotEmpty()) Text(node.tags.joinToString(" · "))
                Text(if (node.accepting) "Admission activée" else "Admission suspendue")
                node.lastSeen?.let { Text("Dernier contact : ${Date(it)}") }
                if (!node.revoked) Row {
                    TextButton(onClick = { editing = node }) { Text("Configurer") }
                    if (!node.local) TextButton(onClick = { revoking = node }) { Text("Révoquer") }
                }
            }
        }
    }
    recovery?.let { settings ->
        RecoveryEditor(settings, state, { recovery = null }) { value ->
            vm.perform { api.request("PUT", "/nodes/settings", wireJson.encodeToJsonElement(value)); recovery = null }
        }
    }
    editing?.let { node ->
        NodeEditor(node, state, { editing = null }) { config ->
            vm.perform {
                api.request("PUT", "/nodes/${node.id}", wireJson.encodeToJsonElement(config))
                editing = null
                load()
            }
        }
    }
    revoking?.let { node ->
        Confirm("Révoquer ${node.name} ?", "Cette machine perdra son accès au master. Une nouvelle inscription sera nécessaire.", state.busy, state.error, { revoking = null }) {
            vm.perform {
                api.request("POST", "/nodes/${node.id}/revoke")
                revoking = null
                load()
            }
        }
    }
}

@Composable
private fun NodeEditor(node: ExecutionNode, state: Workspace, dismiss: () -> Unit, save: (NodeConfiguration) -> Unit) {
    var name by remember(node.id) { mutableStateOf(node.name) }
    var tags by remember(node.id) { mutableStateOf(node.tags.joinToString(", ")) }
    var accepting by remember(node.id) { mutableStateOf(node.accepting) }
    var cpu by remember(node.id) { mutableStateOf(node.limits.cpu.toString()) }
    var memory by remember(node.id) { mutableStateOf(node.limits.memoryMiB.toString()) }
    var disk by remember(node.id) { mutableStateOf(node.limits.diskMiB.toString()) }
    Editor(
        "Configurer la node", state.busy, state.error, close = dismiss,
        valid = name.isNotBlank() && (cpu.toIntOrNull() ?: 0) > 0 && (memory.toLongOrNull() ?: 0) >= 128 && (disk.toLongOrNull() ?: 0) >= 128,
        save = { save(NodeConfiguration(name, accepting, tags.split(',').map(String::trim).filter(String::isNotEmpty), NodeResources(cpu.toInt(), memory.toLong(), disk.toLong()))) },
    ) {
        Field("Nom", name, { name = it }, enabled = !state.busy)
        Field("Tags séparés par des virgules", tags, { tags = it }, enabled = !state.busy)
        Field("Plafond CPU", cpu, { cpu = it }, enabled = !state.busy)
        Field("Plafond RAM (Mio)", memory, { memory = it }, enabled = !state.busy)
        Field("Plafond disque (Mio)", disk, { disk = it }, enabled = !state.busy)
        Toggle("Accepter de nouveaux travaux", accepting, { if (!state.busy) accepting = it })
    }
}

@Composable
private fun RecoveryEditor(settings: NodeBackupSettings, state: Workspace, dismiss: () -> Unit, save: (NodeBackupSettings) -> Unit) {
    var destination by remember { mutableStateOf(settings.destination) }
    var interval by remember { mutableStateOf(settings.intervalSeconds.toString()) }
    var disconnect by remember { mutableStateOf(settings.disconnectTimeoutSeconds.toString()) }
    var shutdown by remember { mutableStateOf(settings.shutdownTimeoutSeconds.toString()) }
    var wait by remember { mutableStateOf(settings.maxCapacityWaitSeconds.toString()) }
    var retention by remember { mutableStateOf(settings.retention.toString()) }
    var budget by remember { mutableStateOf(settings.budgetMiB.toString()) }
    Editor(
        "Sauvegardes de VM", state.busy, state.error, close = dismiss,
        valid = disconnect.toLongOrNull() in 10L..300L && shutdown.toLongOrNull() in 30L..300L && wait.toLongOrNull() in 0L..3600L && interval.toLongOrNull() in 5L..3600L && retention.toIntOrNull() in 1..100 && budget.toLongOrNull() in 128L..1048576L,
        save = { save(NodeBackupSettings(destination, interval.toLong(), retention.toInt(), budget.toLong(), disconnect.toLong(), shutdown.toLong(), wait.toLong())) },
    ) {
        Text("Les blocs modifiés sont envoyés en arrière-plan après une capture cohérente. La date réellement restaurable est visible dans le chat.")
        Row {
            FilterChip(selected = destination == "master", enabled = !state.busy, onClick = { destination = "master" }, label = { Text("Master") })
            FilterChip(selected = destination == "s3", enabled = !state.busy, onClick = { destination = "s3" }, label = { Text("S3 configuré") })
        }
        Field("Intervalle (secondes)", interval, { interval = it }, enabled = !state.busy)
        Field("Suspension après déconnexion (secondes)", disconnect, { disconnect = it }, enabled = !state.busy)
        Field("Préparation de l’arrêt (secondes)", shutdown, { shutdown = it }, enabled = !state.busy)
        Field("Attente de capacité maximale (secondes)", wait, { wait = it }, enabled = !state.busy)
        Field("Points conservés", retention, { retention = it }, enabled = !state.busy)
        Field("Budget master (Mio)", budget, { budget = it }, enabled = !state.busy)
        Text("Une capture peut dépasser l’intervalle. Le cache des sauvegardes S3 utilise aussi ce budget.")
    }
}
