package dev.leo.manager.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
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
    var editing by remember { mutableStateOf<ExecutionNode?>(null) }
    var revoking by remember { mutableStateOf<ExecutionNode?>(null) }
    suspend fun load() { nodes = vm.api.get("/nodes") }
    Poll("nodes", 10_000) {
        try { load() } catch (e: Exception) { vm.report(e) }
    }
    Page {
        Heading("Nodes", "Vos machines de confiance. Le GPU est différé.")
        Text("L’inscription est disponible. L’exécution distante, la migration et les sauvegardes sont en développement ; les nodes inscrites ne lancent pas encore de conversations.")
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
                TextButton(onClick = { enrollment = null }) { Text("Masquer") }
            }
        }
        nodes.forEach { node ->
            Panel {
                Text(node.name, style = MaterialTheme.typography.titleLarge)
                Text(when (node.status) { "online" -> "Connectée"; "offline" -> "Déconnectée"; "revoked" -> "Révoquée"; "local" -> "Runner local"; else -> node.status })
                Text("${node.capabilities.os} · ${node.capabilities.arch} · KVM ${if (node.capabilities.kvm) "disponible" else "indisponible"}")
                Text("Détecté : ${node.capabilities.cpu} CPU · ${node.capabilities.memoryMiB} Mio RAM · ${node.capabilities.diskMiB} Mio disque")
                if (node.runtimeId.isNotBlank()) Text("Runtime : ${node.runtimeId}")
                Text("${node.limits.cpu} CPU · ${node.limits.memoryMiB} Mio RAM · ${node.limits.diskMiB} Mio disque autorisés")
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
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text("Configurer la node") },
        text = {
            Column(Modifier.heightIn(max = 440.dp).verticalScroll(rememberScrollState())) {
                Field("Nom", name, { name = it })
                Field("Tags séparés par des virgules", tags, { tags = it })
                Field("Plafond CPU", cpu, { cpu = it })
                Field("Plafond RAM (Mio)", memory, { memory = it })
                Field("Plafond disque (Mio)", disk, { disk = it })
                Toggle("Accepter de nouveaux travaux", accepting, { accepting = it })
                state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            }
        },
        confirmButton = {
            TextButton(enabled = !state.busy && name.isNotBlank() && (cpu.toIntOrNull() ?: 0) > 0 && (memory.toLongOrNull() ?: 0) >= 128 && (disk.toLongOrNull() ?: 0) >= 128, onClick = {
                save(NodeConfiguration(name, accepting, tags.split(',').map(String::trim).filter(String::isNotEmpty), NodeResources(cpu.toInt(), memory.toLong(), disk.toLong())))
            }) { Text("Enregistrer") }
        },
        dismissButton = { TextButton(onClick = dismiss) { Text("Annuler") } },
    )
}
