package dev.leo.manager.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import dev.leo.manager.data.*
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.put
import java.util.Date

private const val UNINSTALL_COMMAND = "sudo systemctl disable --now leo-node; sudo docker rm -f leo-execution-node; sudo rm -rf /etc/systemd/system/leo-node.service /opt/leo-node /var/lib/leo-node"

@Composable
fun NodesScreen(vm: LeoViewModel, state: Workspace) {
    var nodes by remember { mutableStateOf<List<ExecutionNode>>(emptyList()) }
    var name by remember { mutableStateOf("") }
    // Never persist enrollment secrets in saved instance state.
    var enrollment by remember { mutableStateOf<NodeEnrollment?>(null) }
    // Machines known when the code was created, to recognise the newly connected one.
    var knownBeforeEnrollment by remember { mutableStateOf(emptySet<String>()) }
    var recovery by remember { mutableStateOf<NodeBackupSettings?>(null) }
    var editing by remember { mutableStateOf<ExecutionNode?>(null) }
    var revoking by remember { mutableStateOf<ExecutionNode?>(null) }
    var granting by remember { mutableStateOf<ExecutionNode?>(null) }
    var advanced by remember { mutableStateOf(false) }
    var showRevoked by remember { mutableStateOf(false) }
    suspend fun load() {
        nodes = vm.api.get("/nodes")
        if (enrollment == null) return
        nodes.firstOrNull { !it.local && !it.revoked && it.id !in knownBeforeEnrollment }?.let {
            enrollment = null
            vm.notify("${it.name} est connectée")
            granting = it
        }
    }
    Poll("nodes", 10_000) {
        try { load() } catch (e: Exception) { vm.report(e) }
    }
    val revoked = nodes.filter { it.revoked }
    Page {
        Heading("Nodes", "Vos machines Linux de confiance. Chaque conversation y tourne dans sa propre VM Firecracker. Le GPU n’est pas encore disponible.")
        Panel {
            Text("Ajouter une machine", style = MaterialTheme.typography.titleMedium)
            Field("Nom de la machine", name, { name = it })
            Button(enabled = !state.busy && name.isNotBlank(), onClick = {
                knownBeforeEnrollment = nodes.map { it.id }.toSet()
                vm.perform {
                    enrollment = api.send("POST", "/nodes/enrollments", buildJsonObject { put("name", name) })
                }
            }) { Text("Créer un code d’inscription") }
            enrollment?.let {
                val command = it.installCommand
                if (command != null) {
                    Text("1. Sur la machine Linux (x86-64 avec KVM, Docker, curl et systemd), lancez :")
                    Code(command)
                    CopyButton("Copier la commande", command)
                    Text("2. Saisissez ce code à usage unique quand il est demandé. Il expire le ${Date(it.expiresAt)}.")
                } else {
                    Text("Installation assistée indisponible : le master n’a pas d’image de node épinglée. Définissez LEO_NODE_IMAGE sur le master avec l’image déployée et son digest (image@sha256:…), puis créez un nouveau code.")
                    Text("En attendant, une machine qui a déjà le binaire leo correspondant peut s’inscrire avec leo node-enroll et ce code à usage unique, qui expire le ${Date(it.expiresAt)} :")
                }
                Code(it.code)
                CopyButton("Copier le code", it.code)
                Text("3. Cette page détecte la machine dès qu’elle se connecte et demande quels agents peuvent l’utiliser.", style = MaterialTheme.typography.bodySmall)
                TextButton(onClick = { enrollment = null }) { Text("Masquer") }
            }
        }
        nodes.filter { showRevoked || !it.revoked }.forEach { node ->
            Panel {
                Text(node.name, style = MaterialTheme.typography.titleLarge)
                Text(when (node.status) { "online" -> "Connectée"; "offline" -> "Déconnectée"; "revoked" -> "Révoquée"; "local" -> "Runner du master"; else -> node.status })
                if (node.revoked) {
                    Text("Cette machine n’a plus accès. Réinscrivez-la avec un nouveau code pour la reconnecter.")
                    return@Panel
                }
                Text("Disponible : ${formatResources(node.available ?: node.limits)}")
                Text("Autorisé : ${formatResources(node.limits)}" + (node.reserved?.let { " · réservé : ${formatResources(it)}" } ?: ""), style = MaterialTheme.typography.bodySmall)
                Text("Détecté : ${node.capabilities.cpu} CPU · ${formatMiB(node.capabilities.memoryMiB)} RAM · ${formatMiB(node.capabilities.diskMiB)} disque · KVM ${if (node.capabilities.kvm) "disponible" else "indisponible"}", style = MaterialTheme.typography.bodySmall)
                if (node.agents.isNotEmpty()) Text("Utilisée par ${node.agents.joinToString(", ") { if (it.allNodes) "${it.name} (toutes les nodes)" else it.name }}")
                node.maintenance?.let { Text(if (it == "draining") "Mise en pause et sauvegarde des conversations pour une mise à jour" else "Prête à redémarrer pour la mise à jour")
                    node.maintenanceError?.let { message -> Text(message, color = MaterialTheme.colorScheme.error) }
                }
                nodeDiagnostics(node).forEach { Text("• $it", color = MaterialTheme.colorScheme.error) }
                if (node.tags.isNotEmpty()) Text(node.tags.joinToString(" · "))
                if (node.systemTags.isNotEmpty()) Text("Tags détectés : ${node.systemTags.joinToString(" · ")}", style = MaterialTheme.typography.bodySmall)
                node.lastSeen?.let { Text("Dernier contact : ${Date(it)}", style = MaterialTheme.typography.bodySmall) }
                node.imageDigest?.let { Text("Version : $it", style = MaterialTheme.typography.bodySmall) }
                Row {
                    if (node.agents.isEmpty()) Button(onClick = { granting = node }) { Text("Choisir les agents") }
                    else TextButton(onClick = { granting = node }) { Text("Agents") }
                    TextButton(onClick = { editing = node }) { Text("Configurer") }
                    if (!node.local) TextButton(onClick = { revoking = node }) { Text("Révoquer") }
                }
            }
        }
        if (revoked.isNotEmpty()) TextButton(onClick = { showRevoked = !showRevoked }) {
            Text(if (showRevoked) "Masquer les machines révoquées" else "Afficher les machines révoquées (${revoked.size})")
        }
        TextButton(onClick = { advanced = !advanced }) { Text(if (advanced) "Masquer les réglages avancés" else "Avancé : sauvegardes et délais") }
        if (advanced) Panel {
            Text("Fréquence des sauvegardes des VM, destination des points de reprise et délais avant mise en pause. Les valeurs par défaut conviennent à la plupart des installations.", style = MaterialTheme.typography.bodySmall)
            TextButton(onClick = { vm.perform { recovery = api.get("/nodes/settings") } }) { Text("Configurer les sauvegardes") }
        }
    }
    recovery?.let { settings ->
        RecoveryEditor(settings, state, { recovery = null }) { value ->
            vm.perform { api.request("PUT", "/nodes/settings", wireJson.encodeToJsonElement(value.copy(s3Configured = null))); recovery = null }
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
    granting?.let { node ->
        AgentGrants(node, state, { granting = null }) { agentIds ->
            vm.perform {
                api.request("PUT", "/nodes/${node.id}/agents", buildJsonObject { put("agentIds", buildJsonArray { agentIds.forEach { add(kotlinx.serialization.json.JsonPrimitive(it)) } }) })
                granting = null
                notify("Accès des agents enregistré")
                load()
                refresh()
            }
        }
    }
    revoking?.let { node ->
        val running = (node.reserved?.cpu ?: 0) > 0
        Confirm(
            "Révoquer ${node.name} ?",
            buildString {
                append("Cette machine perd immédiatement son accès au master et les agents ne peuvent plus l’utiliser.")
                if (running) append(" Les conversations en cours se mettent en pause après le délai de déconnexion, puis reprennent depuis leur dernier point de reprise sur une autre machine autorisée qui a de la capacité. Celles fixées à cette machine, ou sans point de reprise, attendent.")
                append("\n\nLes fichiers restent sur la machine. Pour la désinstaller, lancez dessus :\n$UNINSTALL_COMMAND\n(la dernière commande supprime aussi les disques de conversation conservés sur la machine).")
            },
            state.busy, state.error, { revoking = null },
        ) {
            vm.perform {
                api.request("POST", "/nodes/${node.id}/revoke")
                revoking = null
                load()
            }
        }
    }
}

@Composable
private fun AgentGrants(node: ExecutionNode, state: Workspace, dismiss: () -> Unit, save: (List<String>) -> Unit) {
    val everywhere = node.agents.filter { it.allNodes }.map { it.id }.toSet()
    var selected by remember(node.id) { mutableStateOf(node.agents.filter { !it.allNodes }.map { it.id }.toSet()) }
    Editor("Agents autorisés sur ${node.name}", state.busy, state.error, close = dismiss, save = { save(selected.toList()) }) {
        Text("Les agents choisis peuvent exécuter des conversations sur cette machine. Un tag ou une capacité ne donne jamais d’accès à lui seul.")
        state.agents.forEach { agent ->
            if (agent.id in everywhere) Text("${agent.name} (autorisé sur toutes les nodes)")
            else Toggle(agent.name, agent.id in selected) { if (!state.busy) selected = if (it) selected + agent.id else selected - agent.id }
        }
    }
}

@Composable
private fun NodeEditor(node: ExecutionNode, state: Workspace, dismiss: () -> Unit, save: (NodeConfiguration) -> Unit) {
    var name by remember(node.id) { mutableStateOf(node.name) }
    var tags by remember(node.id) { mutableStateOf(node.tags.joinToString(", ")) }
    var accepting by remember(node.id) { mutableStateOf(node.accepting) }
    var cpu by remember(node.id) { mutableStateOf(node.limits.cpu.toString()) }
    // Ceilings are edited in Gio and stored in Mio.
    var memory by remember(node.id) { mutableStateOf(gib(node.limits.memoryMiB)) }
    var disk by remember(node.id) { mutableStateOf(gib(node.limits.diskMiB)) }
    val memoryMiB = mib(memory)
    val diskMiB = mib(disk)
    Editor(
        "Configurer la node", state.busy, state.error, close = dismiss,
        valid = name.isNotBlank() && (cpu.toIntOrNull() ?: 0) > 0 && (memoryMiB ?: 0) >= 128 && (diskMiB ?: 0) >= 128,
        save = { save(NodeConfiguration(name, accepting, tags.split(',').map(String::trim).filter(String::isNotEmpty), NodeResources(cpu.toInt(), memoryMiB!!, diskMiB!!))) },
    ) {
        Field("Nom", name, { name = it }, enabled = !state.busy)
        Field("Tags séparés par des virgules", tags, { tags = it }, enabled = !state.busy)
        Field("Plafond CPU", cpu, { cpu = it }, enabled = !state.busy)
        Field("Plafond RAM (Gio)", memory, { memory = it }, enabled = !state.busy)
        Field("Plafond disque (Gio)", disk, { disk = it }, enabled = !state.busy)
        Text("Détecté sur cette machine : ${node.capabilities.cpu} CPU · ${formatMiB(node.capabilities.memoryMiB)} RAM · ${formatMiB(node.capabilities.diskMiB)} disque.", style = MaterialTheme.typography.bodySmall)
        Toggle("Accepter de nouveaux travaux", accepting, { if (!state.busy) accepting = it })
    }
}

internal fun gib(mib: Long) = if (mib % 1024 == 0L) (mib / 1024).toString() else "%.2f".format(java.util.Locale.ROOT, mib / 1024.0)
internal fun mib(gib: String) = gib.replace(',', '.').toDoubleOrNull()?.let { Math.round(it * 1024) }

@Composable
private fun RecoveryEditor(settings: NodeBackupSettings, state: Workspace, dismiss: () -> Unit, save: (NodeBackupSettings) -> Unit) {
    var destination by remember { mutableStateOf(settings.destination) }
    var interval by remember { mutableStateOf(settings.intervalSeconds.toString()) }
    var disconnect by remember { mutableStateOf(settings.disconnectTimeoutSeconds.toString()) }
    var shutdown by remember { mutableStateOf(settings.shutdownTimeoutSeconds.toString()) }
    var wait by remember { mutableStateOf(settings.maxCapacityWaitSeconds.toString()) }
    var retention by remember { mutableStateOf(settings.retention.toString()) }
    var budget by remember { mutableStateOf(gib(settings.budgetMiB)) }
    val s3 = settings.s3Configured != false
    Editor(
        "Sauvegardes de VM", state.busy, state.error, close = dismiss,
        valid = disconnect.toLongOrNull() in 10L..300L && shutdown.toLongOrNull() in 30L..300L && wait.toLongOrNull() in 0L..3600L && interval.toLongOrNull() in 5L..3600L && retention.toIntOrNull() in 1..100 && mib(budget) in 128L..1048576L,
        save = { save(NodeBackupSettings(destination, interval.toLong(), retention.toInt(), mib(budget)!!, disconnect.toLong(), shutdown.toLong(), wait.toLong())) },
    ) {
        Text("Les blocs modifiés sont envoyés en arrière-plan après une capture cohérente. La date du dernier point de reprise est visible dans chaque conversation.")
        Row {
            FilterChip(selected = destination == "master", enabled = !state.busy, onClick = { destination = "master" }, label = { Text("Master") })
            FilterChip(selected = destination == "s3", enabled = !state.busy && s3, onClick = { destination = "s3" }, label = { Text(if (s3) "S3" else "S3 (non configuré)") })
        }
        if (!s3) Text("Configurez S3 sur le serveur, comme pour l’archivage des conversations, pour y stocker les points de reprise.", style = MaterialTheme.typography.bodySmall)
        Field("Intervalle (secondes)", interval, { interval = it }, enabled = !state.busy)
        Field("Suspension après déconnexion (secondes)", disconnect, { disconnect = it }, enabled = !state.busy)
        Field("Préparation de l’arrêt (secondes)", shutdown, { shutdown = it }, enabled = !state.busy)
        Field("Attente de capacité maximale (secondes)", wait, { wait = it }, enabled = !state.busy)
        Field("Points conservés", retention, { retention = it }, enabled = !state.busy)
        Field("Budget master (Gio)", budget, { budget = it }, enabled = !state.busy)
        Text("Une capture peut dépasser l’intervalle. Le cache des sauvegardes S3 utilise aussi ce budget.")
    }
}
