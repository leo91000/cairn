package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import java.util.Date
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.put

private const val UNINSTALL_COMMAND =
    "sudo systemctl disable --now cairn-node; sudo docker rm -f cairn-execution-node; sudo rm -rf /etc/systemd/system/cairn-node.service /opt/cairn-node /var/lib/cairn-node"

@Composable
fun NodesScreen(vm: CairnViewModel, state: Workspace) {
    var nodes by remember { mutableStateOf<List<ExecutionNode>>(emptyList()) }
    var adding by remember { mutableStateOf(false) }
    var storage by remember { mutableStateOf<ExecutionNode?>(null) }
    var name by remember { mutableStateOf("") }
    // Never persist enrollment secrets in saved instance state.
    var enrollment by remember { mutableStateOf<NodeEnrollment?>(null) }
    // Machines known when the code was created, to recognise the newly connected one.
    var knownBeforeEnrollment by remember { mutableStateOf(emptySet<String>()) }
    var recovery by remember { mutableStateOf<NodeSyncSettings?>(null) }
    var editing by remember { mutableStateOf<ExecutionNode?>(null) }
    var revoking by remember { mutableStateOf<ExecutionNode?>(null) }
    var granting by remember { mutableStateOf<ExecutionNode?>(null) }
    var cleaning by remember { mutableStateOf<ExecutionNode?>(null) }
    var advanced by remember { mutableStateOf(false) }
    var showRevoked by remember { mutableStateOf(false) }
    suspend fun load() {
        nodes = vm.api.get("/nodes")
        if (enrollment == null) return
        nodes
            .firstOrNull { !it.local && !it.revoked && it.id !in knownBeforeEnrollment }
            ?.let {
                enrollment = null
                adding = false
                vm.notify("${it.name} est connectée")
                granting = it
            }
    }
    Poll("nodes", 10_000) {
        try {
            load()
        } catch (e: Exception) {
            vm.report(e)
        }
    }
    val revoked = nodes.filter { it.revoked }
    Page {
        Heading(
            "Nodes",
            "Gérez vos machines, leur capacité et les agents autorisés.",
        )
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text(
                nodes.count { !it.revoked }.let { if (it == 1) "1 machine" else "$it machines" },
                Modifier.weight(1f),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            FilledTonalButton(onClick = { adding = true }) { Text("Ajouter une machine") }
        }
        nodes
            .filter { showRevoked || !it.revoked }
            .forEach { node ->
                Panel {
                    Text(node.name, style = MaterialTheme.typography.titleMedium)
                    Text(
                        when (node.status) {
                            "online" -> "Connectée"
                            "offline" -> "Déconnectée"
                            "revoked" -> "Révoquée"
                            "local" -> "Runner du master"
                            else -> node.status
                        }
                    )
                    if (node.revoked) {
                        Text(
                            "Cette machine n’a plus accès. Réinscrivez-la avec un nouveau code pour la reconnecter."
                        )
                        return@Panel
                    }
                    val available = node.limits
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        NodeCapacity(
                            "CPU partagés",
                            available.cpu.toString(),
                            "pour tous les slots",
                            Modifier.weight(1f),
                        )
                        NodeCapacity(
                            "RAM partagée",
                            formatMiB(available.memoryMiB),
                            node.usage?.let { "${formatMiB(it.memoryMiB)} utilisés" }
                                ?: "pour tous les slots",
                            Modifier.weight(1f),
                        )
                        NodeCapacity(
                            "Disque partagé",
                            formatMiB(available.diskMiB),
                            node.usage?.let { "${formatMiB(it.diskMiB)} utilisés" }
                                ?: "pour tous les slots",
                            Modifier.weight(1f),
                        )
                    }
                    Text(
                        "${node.availableSlots} slots libres sur ${node.slots} · ${node.occupiedSlots} occupés"
                    )
                    if (node.agents.isNotEmpty())
                        Text(
                            "Utilisée par ${node.agents.joinToString(", ") { if (it.allNodes) "${it.name} (toutes les nodes)" else it.name }}"
                        )
                    if (node.staleDisks.count > 0)
                        Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                            Text(
                                "Anciens disques : ${formatMiB(node.staleDisks.diskMiB)} (${node.staleDisks.count} conversation${if (node.staleDisks.count > 1) "s" else ""})",
                                Modifier.weight(1f),
                            )
                            TextButton(onClick = { cleaning = node }) { Text("Libérer") }
                        }
                    node.maintenance?.let {
                        Text(
                            if (it == "draining")
                                "Mise en pause et sauvegarde des conversations pour une mise à jour"
                            else "Prête à redémarrer pour la mise à jour"
                        )
                        node.maintenanceError?.let { message ->
                            Text(message, color = MaterialTheme.colorScheme.error)
                        }
                    }
                    nodeDiagnostics(node).forEach {
                        Text("• $it", color = MaterialTheme.colorScheme.error)
                    }
                    if (node.tags.isNotEmpty()) Text(node.tags.joinToString(" · "))
                    var technical by remember(node.id) { mutableStateOf(false) }
                    TextButton(onClick = { technical = !technical }) {
                        Text(if (technical) "Masquer les détails" else "Détails techniques")
                    }
                    if (technical) {
                        Text(
                            "Détecté : ${node.capabilities.cpu} CPU · ${formatMiB(node.capabilities.memoryMiB)} RAM · ${formatMiB(node.capabilities.diskMiB)} disque · KVM ${if (node.capabilities.kvm) "disponible" else "indisponible"}",
                            style = MaterialTheme.typography.bodySmall,
                        )
                        node.usage?.let {
                            Text(
                                "Utilisé : ${formatMiB(it.memoryMiB)} RAM · ${formatMiB(it.diskMiB)} disque",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                        if (node.systemTags.isNotEmpty())
                            Text(
                                "Tags détectés : ${node.systemTags.joinToString(" · ")}",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        node.lastSeen?.let {
                            Text(
                                "Dernier contact : ${Date(it)}",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                        node.imageDigest?.let {
                            Text("Version : $it", style = MaterialTheme.typography.bodySmall)
                        }
                    }
                    HorizontalDivider()
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        if (node.agents.isEmpty())
                            Button(onClick = { granting = node }) { Text("Choisir les agents") }
                        else TextButton(onClick = { granting = node }) { Text("Agents") }
                        TextButton(onClick = { editing = node }) { Text("Configurer") }
                        TextButton(onClick = { storage = node }) { Text("Stockage") }
                        if (!node.local)
                            TextButton(onClick = { revoking = node }) { Text("Révoquer") }
                    }
                }
            }
        if (revoked.isNotEmpty())
            TextButton(onClick = { showRevoked = !showRevoked }) {
                Text(
                    if (showRevoked) "Masquer les machines révoquées"
                    else "Afficher les machines révoquées (${revoked.size})"
                )
            }
        TextButton(onClick = { advanced = !advanced }) {
            Text(
                if (advanced) "Masquer les réglages avancés"
                else "Avancé : synchronisation et délais"
            )
        }
        if (advanced)
            Panel {
                Text(
                    "Fréquence de synchronisation S3 des disques et délais avant mise en pause. Les valeurs par défaut conviennent à la plupart des installations.",
                    style = MaterialTheme.typography.bodySmall,
                )
                TextButton(onClick = { vm.perform { recovery = api.get("/nodes/settings") } }) {
                    Text("Configurer la synchronisation")
                }
            }
    }
    if (adding)
        DetailSheet("Ajouter une machine", { adding = false }) {
            Text(
                "Connectez une machine Linux, puis choisissez les agents autorisés.",
                style = MaterialTheme.typography.bodyMedium,
            )
            Field("Nom de la machine", name, { name = it })
            Button(
                enabled = !state.busy && name.isNotBlank(),
                onClick = {
                    knownBeforeEnrollment = nodes.map { it.id }.toSet()
                    vm.perform {
                        enrollment =
                            api.send(
                                "POST",
                                "/nodes/enrollments",
                                buildJsonObject { put("name", name) },
                            )
                    }
                },
            ) {
                Text("Créer un code d’inscription")
            }
            enrollment?.let {
                val command = it.installCommand
                if (command != null) {
                    Text(
                        "1. Sur la machine Linux (x86-64 avec KVM, Docker, curl et systemd), lancez :"
                    )
                    Code(command)
                    CopyButton("Copier la commande", command)
                    Text(
                        "2. Saisissez ce code à usage unique quand il est demandé. Il expire le ${Date(it.expiresAt)}."
                    )
                } else {
                    Text(
                        "Installation assistée indisponible : le master n’a pas d’image de node épinglée. Définissez CAIRN_NODE_IMAGE sur le master avec l’image déployée et son digest (image@sha256:…), puis créez un nouveau code."
                    )
                    Text(
                        "En attendant, une machine qui a déjà le binaire cairn correspondant peut s’inscrire avec cairn node-enroll et ce code à usage unique, qui expire le ${Date(it.expiresAt)} :"
                    )
                }
                Code(it.code)
                CopyButton("Copier le code", it.code)
                Text(
                    "3. Cette page détecte la machine dès qu’elle se connecte et demande quels agents peuvent l’utiliser.",
                    style = MaterialTheme.typography.bodySmall,
                )
                TextButton(
                    onClick = {
                        enrollment = null
                        adding = false
                    }
                ) {
                    Text("Masquer")
                }
            }
        }
    recovery?.let { settings ->
        RecoveryEditor(settings, state, { recovery = null }) { value ->
            vm.perform {
                api.request(
                    "PUT",
                    "/nodes/settings",
                    wireJson.encodeToJsonElement(value.copy(s3Configured = null)),
                )
                recovery = null
            }
        }
    }
    storage?.let { node ->
        NodeStorageEditor(node, state, { storage = null }) { policy ->
            vm.perform {
                api.request(
                    "PUT",
                    "/nodes/${node.id}/storage",
                    wireJson.encodeToJsonElement(policy),
                )
                storage = null
                load()
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
    granting?.let { node ->
        AgentGrants(node, state, { granting = null }) { agentIds ->
            vm.perform {
                api.request(
                    "PUT",
                    "/nodes/${node.id}/agents",
                    buildJsonObject {
                        put(
                            "agentIds",
                            buildJsonArray {
                                agentIds.forEach {
                                    add(kotlinx.serialization.json.JsonPrimitive(it))
                                }
                            },
                        )
                    },
                )
                granting = null
                notify("Accès des agents enregistré")
                load()
                refresh()
            }
        }
    }
    cleaning?.let { node ->
        Confirm(
            "Libérer les anciens disques de ${node.name} ?",
            "Ces disques appartiennent à des conversations qui tournent maintenant sur une autre machine, ou sont d’anciennes copies mises de côté lors d’une reprise. Après une bascule, un ancien disque peut contenir des changements plus récents que le point de reprise utilisé. La suppression est définitive. La machine doit être en ligne ; les disques des conversations en cours ou en déplacement sont conservés.",
            state.busy,
            state.error,
            { cleaning = null },
        ) {
            vm.perform {
                val result: StaleDiskCleanup =
                    api.send("POST", "/nodes/${node.id}/stale-disks/delete", buildJsonObject {})
                cleaning = null
                notify(
                    if (result.failed > 0)
                        "${formatMiB(result.freedMiB)} libérés ; ${result.failed} disques utilisés ou machine injoignable"
                    else "${formatMiB(result.freedMiB)} libérés"
                )
                load()
            }
        }
    }
    revoking?.let { node ->
        val running = node.occupiedSlots > 0
        Confirm(
            "Révoquer ${node.name} ?",
            buildString {
                append(
                    "Cette machine perd immédiatement son accès au master et les agents ne peuvent plus l’utiliser."
                )
                if (running)
                    append(
                        " Les conversations en cours se mettent en pause après le délai de déconnexion, puis reprennent depuis leur dernier point de reprise sur une autre machine autorisée qui a de la capacité. Celles fixées à cette machine, ou sans point de reprise, attendent."
                    )
                append(
                    "\n\nLes fichiers restent sur la machine. Pour la désinstaller, lancez dessus :\n$UNINSTALL_COMMAND\n(la dernière commande supprime aussi les disques de conversation conservés sur la machine)."
                )
            },
            state.busy,
            state.error,
            { revoking = null },
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
private fun AgentGrants(
    node: ExecutionNode,
    state: Workspace,
    dismiss: () -> Unit,
    save: (List<String>) -> Unit,
) {
    val everywhere = node.agents.filter { it.allNodes }.map { it.id }.toSet()
    var selected by
        remember(node.id) {
            mutableStateOf(node.agents.filter { !it.allNodes }.map { it.id }.toSet())
        }
    Editor(
        "Agents autorisés sur ${node.name}",
        state.busy,
        state.error,
        close = dismiss,
        save = { save(selected.toList()) },
    ) {
        Text(
            "Les agents choisis peuvent exécuter des conversations sur cette machine. Un tag ou une capacité ne donne jamais d’accès à lui seul."
        )
        Panel {
            state.agents.forEach { agent ->
                if (agent.id in everywhere) Text("${agent.name} (autorisé sur toutes les nodes)")
                else
                    Toggle(agent.name, agent.id in selected) {
                        if (!state.busy)
                            selected = if (it) selected + agent.id else selected - agent.id
                    }
            }
        }
    }
}

@Composable
private fun NodeEditor(
    node: ExecutionNode,
    state: Workspace,
    dismiss: () -> Unit,
    save: (NodeConfiguration) -> Unit,
) {
    var name by remember(node.id) { mutableStateOf(node.name) }
    var tags by remember(node.id) { mutableStateOf(node.tags.joinToString(", ")) }
    var accepting by remember(node.id) { mutableStateOf(node.accepting) }
    var slots by remember(node.id) { mutableStateOf(node.slots.toString()) }
    var cpu by remember(node.id) { mutableStateOf(node.limits.cpu.toString()) }
    // Ceilings are edited in Gio and stored in Mio.
    var memory by remember(node.id) { mutableStateOf(gib(node.limits.memoryMiB)) }
    var disk by remember(node.id) { mutableStateOf(gib(node.limits.diskMiB)) }
    val memoryMiB = mib(memory)
    val diskMiB = mib(disk)
    Editor(
        "Configurer la node",
        state.busy,
        state.error,
        close = dismiss,
        valid =
            name.isNotBlank() &&
                (cpu.toIntOrNull() ?: 0) > 0 &&
                (slots.toIntOrNull() ?: 0) in 1..4096 &&
                (memoryMiB ?: 0) >= 128 &&
                (diskMiB ?: 0) >= 128,
        save = {
            save(
                NodeConfiguration(
                    name,
                    accepting,
                    tags.split(',').map(String::trim).filter(String::isNotEmpty),
                    NodeResources(cpu.toInt(), memoryMiB!!, diskMiB!!),
                    slots.toInt(),
                )
            )
        },
    ) {
        Heading(node.name, "Identité et capacité de cette machine")
        Panel {
            Text("Identité", style = MaterialTheme.typography.titleSmall)
            Field("Nom", name, { name = it }, enabled = !state.busy)
            Field("Tags séparés par des virgules", tags, { tags = it }, enabled = !state.busy)
        }
        Panel {
            Text("Slots et budgets partagés", style = MaterialTheme.typography.titleSmall)
            Field("Slots d’exécution", slots, { slots = it }, enabled = !state.busy)
            Field("Budget CPU partagé", cpu, { cpu = it }, enabled = !state.busy)
            Field("Budget RAM partagé (Gio)", memory, { memory = it }, enabled = !state.busy)
            Field("Budget disque partagé (Gio)", disk, { disk = it }, enabled = !state.busy)
            Text(
                "Détecté sur cette machine : ${node.capabilities.cpu} CPU · ${formatMiB(node.capabilities.memoryMiB)} RAM · ${formatMiB(node.capabilities.diskMiB)} disque.",
                style = MaterialTheme.typography.bodySmall,
            )
        }
        Toggle("Accepter de nouveaux travaux", accepting, { if (!state.busy) accepting = it })
    }
}

internal fun gib(mib: Long) =
    if (mib % 1024 == 0L) (mib / 1024).toString()
    else "%.2f".format(java.util.Locale.ROOT, mib / 1024.0)

internal fun mib(gib: String) =
    gib.replace(',', '.').toDoubleOrNull()?.let { Math.round(it * 1024) }

@Composable
private fun RecoveryEditor(
    settings: NodeSyncSettings,
    state: Workspace,
    dismiss: () -> Unit,
    save: (NodeSyncSettings) -> Unit,
) {
    var interval by remember { mutableStateOf(settings.intervalSeconds.toString()) }
    var disconnect by remember { mutableStateOf(settings.disconnectTimeoutSeconds.toString()) }
    var shutdown by remember { mutableStateOf(settings.shutdownTimeoutSeconds.toString()) }
    var wait by remember { mutableStateOf(settings.maxCapacityWaitSeconds.toString()) }
    var budget by remember { mutableStateOf(gib(settings.budgetMiB)) }
    val s3 = settings.s3Configured != false
    Editor(
        "Synchronisation S3",
        state.busy,
        state.error,
        close = dismiss,
        valid =
            disconnect.toLongOrNull() in 10L..300L &&
                shutdown.toLongOrNull() in 30L..300L &&
                wait.toLongOrNull() in 0L..3600L &&
                interval.toLongOrNull() in 5L..3600L &&
                mib(budget) in 128L..1048576L,
        save = {
            save(
                NodeSyncSettings(
                    interval.toLong(),
                    mib(budget)!!,
                    disconnect.toLong(),
                    shutdown.toLong(),
                    wait.toLong(),
                )
            )
        },
    ) {
        Text(
            "Les blocs modifiés sont envoyés en arrière-plan après une capture cohérente. La dernière synchronisation est visible dans chaque conversation."
        )
        if (!s3)
            Text(
                "Configurez S3 sur le serveur pour synchroniser les disques.",
                style = MaterialTheme.typography.bodySmall,
            )
        Panel {
            Text("Fréquence et délais", style = MaterialTheme.typography.titleSmall)
            Field("Intervalle (secondes)", interval, { interval = it }, enabled = !state.busy)
            Field(
                "Suspension après déconnexion (secondes)",
                disconnect,
                { disconnect = it },
                enabled = !state.busy,
            )
            Field(
                "Préparation de l’arrêt (secondes)",
                shutdown,
                { shutdown = it },
                enabled = !state.busy,
            )
            Field(
                "Attente de capacité maximale (secondes)",
                wait,
                { wait = it },
                enabled = !state.busy,
            )
            Field(
                "Budget du cache de publication (Gio)",
                budget,
                { budget = it },
                enabled = !state.busy,
            )
        }
        Text("Une capture peut dépasser l’intervalle. Le cache de publication utilise ce budget.")
    }
}

@Composable
private fun NodeCapacity(label: String, value: String, limit: String, modifier: Modifier) {
    Surface(
        modifier,
        shape = MaterialTheme.shapes.medium,
        color = MaterialTheme.colorScheme.surfaceContainerHigh,
    ) {
        Column(Modifier.padding(10.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(
                label,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(value, style = MaterialTheme.typography.titleMedium)
            Text(
                limit,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun NodeStorageEditor(
    node: ExecutionNode,
    state: Workspace,
    dismiss: () -> Unit,
    save: (NodeStoragePolicy) -> Unit,
) {
    val policy = node.storage ?: NodeStoragePolicy()
    var cache by remember { mutableStateOf(policy.cacheMiB.toString()) }
    var reserve by remember { mutableStateOf(policy.reserveMiB.toString()) }
    var percent by remember { mutableStateOf(policy.reservePercent.toString()) }
    var interval by remember { mutableStateOf(policy.backupSeconds.toString()) }
    var dirty by remember { mutableStateOf(policy.maxDirtySeconds.toString()) }
    Editor(
        "Stockage · ${node.name}",
        state.busy,
        state.error,
        close = dismiss,
        valid =
            cache.toLongOrNull() in 0L..16777216L &&
                reserve.toLongOrNull() in 64L..16777216L &&
                percent.toIntOrNull() in 1..50 &&
                interval.toLongOrNull() in 5L..3600L &&
                dirty.toLongOrNull() in (interval.toLongOrNull() ?: 5L)..86400L,
        save = {
            save(
                NodeStoragePolicy(
                    cache.toLong(),
                    reserve.toLong(),
                    percent.toInt(),
                    interval.toLong(),
                    dirty.toLong(),
                )
            )
        },
    ) {
        Text(
            "Les fichiers sont sauvegardés sur S3 et chargés à la demande. Libérer le cache ne ferme pas les conversations."
        )
        Panel {
            Text("Disque local", style = MaterialTheme.typography.titleSmall)
            Field(
                "Cache propre (Mio)",
                cache,
                { cache = it },
                enabled = !state.busy,
                keyboardOptions = InputKeyboards.Number,
            )
            Field(
                "Espace libre minimum (Mio)",
                reserve,
                { reserve = it },
                enabled = !state.busy,
                keyboardOptions = InputKeyboards.Number,
            )
            Field(
                "Espace libre minimum (%)",
                percent,
                { percent = it },
                enabled = !state.busy,
                keyboardOptions = InputKeyboards.Number,
            )
            Text(
                "La plus grande réserve s’applique. Le travail non synchronisé reste local.",
                style = MaterialTheme.typography.bodySmall,
            )
        }
        Panel {
            Text("Synchronisation", style = MaterialTheme.typography.titleSmall)
            Field(
                "Synchroniser toutes les (secondes)",
                interval,
                { interval = it },
                enabled = !state.busy,
                keyboardOptions = InputKeyboards.Number,
            )
            Field(
                "Suspendre après (secondes)",
                dirty,
                { dirty = it },
                enabled = !state.busy,
                keyboardOptions = InputKeyboards.Number,
            )
            Text(
                "Suspend les exécutions si leurs modifications restent non synchronisées trop longtemps.",
                style = MaterialTheme.typography.bodySmall,
            )
        }
    }
}
