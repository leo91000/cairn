package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import java.util.Date
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.put

private const val UNINSTALL_COMMAND =
    "sudo systemctl disable --now cairn-node; sudo docker rm -f cairn-execution-node; sudo rm -rf /etc/systemd/system/cairn-node.service /opt/cairn-node /var/lib/cairn-node"

private const val NATIVE_UNINSTALL_COMMAND =
    "sudo sh -c 'if [ -e /etc/systemd/system/cairn-native-node.service ]; then systemctl disable --now cairn-native-node || exit; fi; mounts=\$(findmnt -rno TARGET) || { echo \"findmnt could not list mounts.\" >&2; exit 1; }; printf \"%s\\n\" \"\$mounts\" | grep ^/var/lib/cairn-native/; match=\$?; if [ \"\$match\" -ne 1 ]; then echo \"Mount safety could not be confirmed. Run sudo systemctl start cairn-native-node to release owned conversation disks, then retry.\" >&2; exit 1; fi; rm -rf /opt/cairn-native /var/lib/cairn-native && { userdel cairn-workload || ! id cairn-workload >/dev/null 2>&1; } && rm -f /etc/systemd/system/cairn-native-node.service && systemctl daemon-reload'"

private fun uninstallCommand(node: ExecutionNode) =
    if (node.capabilities.backend == NATIVE_BACKEND) NATIVE_UNINSTALL_COMMAND else UNINSTALL_COMMAND

@Composable
fun NodesScreen(
    vm: CairnViewModel,
    state: Workspace,
    openRun: (String) -> Unit = {},
    openAgents: () -> Unit = {},
) {
    var nodes by remember { mutableStateOf<List<ExecutionNode>>(emptyList()) }
    var adding by remember { mutableStateOf(false) }
    var storage by remember { mutableStateOf<ExecutionNode?>(null) }
    var name by remember { mutableStateOf("") }
    var backend by remember { mutableStateOf("firecracker") }
    var nativeCallers by remember { mutableStateOf<List<NativeCaller>>(emptyList()) }
    // Never persist enrollment secrets in saved instance state.
    var enrollment by remember { mutableStateOf<NodeEnrollment?>(null) }
    // Machines known when the code was created, to recognise the newly connected one.
    var knownBeforeEnrollment by remember { mutableStateOf(emptySet<String>()) }
    var recovery by remember { mutableStateOf<NodeSyncSettings?>(null) }
    var editing by remember { mutableStateOf<ExecutionNode?>(null) }
    var revoking by remember { mutableStateOf<ExecutionNode?>(null) }
    // The last step of adding a machine: who may use it. Opened once, right after enrollment.
    var granting by remember { mutableStateOf<ExecutionNode?>(null) }
    // A new native machine is configured first (native execution, trusted people), then asks
    // who may use it.
    var grantAfterConfigure by remember { mutableStateOf<String?>(null) }
    // The node a guided backend switch replaces, while the new runtime's enrollment is pending.
    var replacing by remember { mutableStateOf<ExecutionNode?>(null) }
    // Previous and new node ids: once the new node's setup ends, retiring the previous is offered.
    var replacement by remember { mutableStateOf<Pair<String, String>?>(null) }
    var retiring by remember { mutableStateOf<Pair<ExecutionNode, ExecutionNode>?>(null) }
    fun offerRetirement(node: String) {
        val (previousId, nextId) = replacement ?: return
        if (nextId != node) return
        replacement = null
        val previous = nodes.find { it.id == previousId && !it.revoked }
        val next = nodes.find { it.id == node }
        if (previous != null && next != null) retiring = previous to next
    }
    var cleaning by remember { mutableStateOf<ExecutionNode?>(null) }
    var advanced by remember { mutableStateOf(false) }
    var showRevoked by remember { mutableStateOf(false) }
    suspend fun load() {
        nodes = vm.api.get("/nodes")
        if (nodes.any { it.capabilities.backend == "native" })
            nativeCallers = vm.api.get("/nodes/native-callers")
        if (enrollment == null) return
        nodes
            .firstOrNull { !it.local && !it.revoked && it.id !in knownBeforeEnrollment }
            ?.let {
                enrollment = null
                adding = false
                vm.notify("${it.name} est connectée")
                replacing?.let { previous ->
                    replacement = previous.id to it.id
                    replacing = null
                }
                if (it.capabilities.backend == NATIVE_BACKEND) {
                    grantAfterConfigure = it.id
                    editing = it
                } else granting = it
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
            "Gérez vos machines et leur capacité. L’accès des agents se règle sur leur fiche.",
        )
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text(
                nodes.count { !it.revoked }.let { if (it == 1) "1 machine" else "$it machines" },
                Modifier.weight(1f),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            FilledTonalButton(
                onClick = {
                    replacing = null
                    adding = true
                }
            ) {
                Text("Ajouter une machine")
            }
        }
        nodes
            .filter { showRevoked || !it.revoked }
            .forEach { node ->
                Panel {
                    Text(node.name, style = MaterialTheme.typography.titleMedium)
                    Text(
                        if (node.capabilities.backend == "native")
                            "Processus natif · GPU partagé · personnes de confiance"
                        else "VM Firecracker",
                        style = MaterialTheme.typography.bodySmall,
                    )
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
                    NodeAgents(node, state, openAgents)
                    node.exclusiveReservation?.let { reservation ->
                        Panel {
                            Text(
                                "Réservation exclusive",
                                style = MaterialTheme.typography.titleSmall,
                            )
                            Text(exclusiveReservationState(reservation.state))
                            Text(reservation.reason)
                            Text(
                                "${reservation.pausedRunIds.size} tâche${if (reservation.pausedRunIds.size == 1) "" else "s"} suspendue${if (reservation.pausedRunIds.size == 1) "" else "s"}"
                            )
                            Text(
                                reservation.expiresAt?.let { "Expire le ${date(it)}" }
                                    ?: "Attente jusqu’au ${date(reservation.waitUntil)}"
                            )
                            if (reservation.renewalPending) Text("Prolongation en attente")
                            if (reservation.minimumAvailableMemoryMiB > 0)
                                Text(
                                    "RAM disponible demandée : ${formatMiB(reservation.minimumAvailableMemoryMiB)}"
                                )
                            reservation.error?.let {
                                Text(it, color = MaterialTheme.colorScheme.error)
                            }
                            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                TextButton(onClick = { openRun(reservation.ownerRunId) }) {
                                    Text("Ouvrir l’exécution")
                                }
                                OutlinedButton(
                                    enabled =
                                        !state.busy &&
                                            reservation.state in
                                                listOf("queued", "acquiring", "acquired"),
                                    onClick = {
                                        vm.perform {
                                            api.request(
                                                "POST",
                                                "/nodes/${segment(node.id)}/exclusive/${segment(reservation.id)}/release",
                                            )
                                            load()
                                            notify("Libération de la réservation demandée")
                                        }
                                    },
                                ) {
                                    Text("Libérer la réservation")
                                }
                            }
                        }
                    }
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
                            "Détecté : ${node.capabilities.cpu} CPU · ${formatMiB(node.capabilities.memoryMiB)} RAM · ${formatMiB(node.capabilities.diskMiB)} disque · ${if (node.capabilities.backend == NATIVE_BACKEND) "ublk ${if (node.capabilities.ublk) "disponible" else "indisponible"}" else "KVM ${if (node.capabilities.kvm) "disponible" else "indisponible"}"}",
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
                        TextButton(onClick = { editing = node }) { Text("Configurer") }
                        TextButton(onClick = { storage = node }) { Text("Stockage") }
                        if (!node.local)
                            TextButton(
                                onClick = {
                                    // Another runtime enrolls as a new node, so its grants and
                                    // checkpoints never mix with the old ones.
                                    replacing = node
                                    backend =
                                        if (node.capabilities.backend == NATIVE_BACKEND)
                                            "firecracker"
                                        else NATIVE_BACKEND
                                    name = node.name
                                    enrollment = null
                                    adding = true
                                }
                            ) {
                                Text(
                                    if (node.capabilities.backend == NATIVE_BACKEND) "Passer en VM"
                                    else "Passer en natif"
                                )
                            }
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
        DetailSheet(
            replacing?.let {
                "Passer ${it.name} en ${if (backend == NATIVE_BACKEND) "natif" else "VM"}"
            } ?: "Ajouter une machine",
            { adding = false },
        ) {
            Text(
                replacing?.let {
                    "Installez le runtime ${if (backend == NATIVE_BACKEND) "natif" else "VM Firecracker"} sur la machine de ${it.name}. Il s’inscrit comme une nouvelle node ; une fois connectée et configurée, Cairn propose de révoquer ${it.name}. Les conversations gardent leur backend d’exécution, et celui de chaque agent se choisit sur sa fiche."
                } ?: "Connectez une machine Linux, puis choisissez qui peut l’utiliser.",
                style = MaterialTheme.typography.bodyMedium,
            )
            Field("Nom de la machine", name, { name = it })
            if (replacing == null)
                Toggle("Exécution native sur l’hôte", backend == "native") {
                    backend = if (it) "native" else "firecracker"
                }
            if (backend == "native")
                Text(
                    "Linux x86-64 avec systemd et le pilote noyau ublk. Le GPU reste utilisable par le bureau, mais les travaux intensifs peuvent le ralentir. L’isolation est plus faible qu’une VM."
                )
            Button(
                enabled = !state.busy && name.isNotBlank(),
                onClick = {
                    knownBeforeEnrollment = nodes.map { it.id }.toSet()
                    vm.perform {
                        enrollment =
                            api.send(
                                "POST",
                                "/nodes/enrollments",
                                buildJsonObject {
                                    put("name", name)
                                    put("backend", backend)
                                },
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
                        if (it.backend == "native")
                            "1. Sur la machine Linux x86-64 avec Python 3, curl, systemd, e2fsprogs et le module noyau ublk_drv, lancez :"
                        else
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
                    if (it.backend == NATIVE_BACKEND)
                        "3. Cette page détecte la machine dès qu’elle se connecte, ouvre sa configuration pour activer l’exécution native et choisir les personnes de confiance, puis demande quels agents peuvent l’utiliser."
                    else
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
        // Dismissing the configuration of a new native machine cancels the last step; only a
        // saved configuration continues to who may use it.
        fun dismiss() {
            editing = null
            if (grantAfterConfigure == node.id) {
                grantAfterConfigure = null
                offerRetirement(node.id)
            }
        }
        NodeEditor(node, state, nativeCallers, ::dismiss) { config ->
            vm.perform {
                api.request("PUT", "/nodes/${node.id}", wireJson.encodeToJsonElement(config))
                load()
                editing = null
                if (grantAfterConfigure == node.id) {
                    grantAfterConfigure = null
                    granting = nodes.find { it.id == node.id } ?: node
                }
            }
        }
    }
    granting?.let { opened ->
        // The page keeps polling: grants made meanwhile, as from an agent page in another window,
        // show up here and are kept.
        val node = nodes.find { it.id == opened.id } ?: opened
        fun later() {
            granting = null
            offerRetirement(node.id)
        }
        FirstAgents(node, state, ::later) { chosen ->
            vm.perform {
                // Only grants the chosen agents: finishing never revokes one, including an agent
                // allowed meanwhile from another window.
                grantNodeAgents(chosen, node.grantedAgents(state.agents)) { body ->
                    api.request("PUT", "/nodes/${node.id}/agents", body)
                }
                granting = null
                notify("${node.name} est prête")
                load()
                refresh()
                offerRetirement(node.id)
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
        val native = node.capabilities.backend == NATIVE_BACKEND
        val command = uninstallCommand(node)
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
                    "\n\nLes fichiers restent sur la machine. Pour la désinstaller, lancez dessus :"
                )
            },
            state.busy,
            state.error,
            { revoking = null },
            details = {
                Code(command)
                CopyButton("Copier la commande", command)
                Text(
                    "Supprimer ces répertoires efface aussi les disques de conversation conservés sur la machine.",
                    style = MaterialTheme.typography.bodySmall,
                )
                if (native)
                    Text(
                        "Si un disque de conversation est encore monté, la commande l’affiche et s’arrête avant de supprimer quoi que ce soit : démarrez cairn-native-node pour que sa reprise libère les disques dont il est propriétaire, puis relancez la commande.",
                        style = MaterialTheme.typography.bodySmall,
                    )
            },
        ) {
            vm.perform {
                api.request("POST", "/nodes/${node.id}/revoke")
                revoking = null
                load()
            }
        }
    }
    retiring?.let { (previous, next) ->
        RetireNode(previous, next, state, { retiring = null }) {
            vm.perform {
                val result: NodeRevocation =
                    api.send(
                        "POST",
                        "/nodes/${previous.id}/revoke",
                        buildJsonObject { put("releasePlacements", true) },
                    )
                retiring = null
                notify(
                    when (result.releasedPlacements) {
                        0 -> "${previous.name} révoquée"
                        1 ->
                            "${previous.name} révoquée ; 1 conversation revient au placement automatique"
                        else ->
                            "${previous.name} révoquée ; ${result.releasedPlacements} conversations reviennent au placement automatique"
                    }
                )
                load()
            }
        }
    }
}

/** The last step of a guided backend switch: revoke the previous node, or keep both for now. */
@Composable
internal fun RetireNode(
    previous: ExecutionNode,
    next: ExecutionNode,
    state: Workspace,
    keep: () -> Unit,
    revoke: () -> Unit,
) {
    val nextNative = next.capabilities.backend == NATIVE_BACKEND
    val previousNative = previous.capabilities.backend == NATIVE_BACKEND
    val agents = previous.agents.filter { !it.allNodes }.map { it.name }
    DetailSheet("Retirer l’ancienne node ${if (previousNative) "native" else "VM"} ?", keep) {
        Text(
            "${next.name} tourne maintenant comme node ${if (nextNative) "native" else "VM"}. Révoquez l’ancienne node ${if (previousNative) "native" else "VM"}, ${previous.name}, pour terminer la bascule : elle perd son accès au master et les agents ne peuvent plus l’utiliser."
        )
        if (agents.isNotEmpty())
            Text(
                "Agents autorisés sur l’ancienne node : ${agents.joinToString(", ")}. Pour les faire tourner sur la nouvelle, passez-les en ${if (nextNative) "exécution native" else "VM Firecracker"} sur leur fiche et autorisez-la."
            )
        Text(
            "Les conversations fixées sur l’ancienne node ou qui la préfèrent reviennent au placement automatique. Les conversations gardent leur backend d’exécution : les existantes continuent sur d’autres nodes ${if (previousNative) "natives" else "VM"}."
        )
        if (previous.occupiedSlots > 0)
            Text(
                "Les conversations en cours se mettent en pause après le délai de déconnexion, puis reprennent depuis leur dernier point de reprise sur une autre machine autorisée qui a de la capacité."
            )
        Text("Puis désinstallez l’ancien runtime sur la machine :")
        Code(uninstallCommand(previous))
        CopyButton("Copier la commande", uninstallCommand(previous))
        Text(
            "Supprimer ces répertoires efface aussi les disques de conversation conservés pour l’ancienne node.",
            style = MaterialTheme.typography.bodySmall,
        )
        if (previousNative)
            Text(
                "Si un disque de conversation est encore monté, la commande l’affiche et s’arrête avant de supprimer quoi que ce soit : démarrez cairn-native-node pour que sa reprise libère les disques dont il est propriétaire, puis relancez-la.",
                style = MaterialTheme.typography.bodySmall,
            )
        state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            TextButton(onClick = keep, enabled = !state.busy) {
                Text("Garder les deux pour l’instant")
            }
            Button(
                onClick = revoke,
                enabled = !state.busy,
                colors =
                    ButtonDefaults.buttonColors(
                        containerColor = MaterialTheme.colorScheme.error,
                        contentColor = MaterialTheme.colorScheme.onError,
                    ),
            ) {
                Text("Révoquer l’ancienne node")
            }
        }
    }
}

/** Who can run here and reserve the machine, read-only: grants are set on each agent's page. */
@Composable
private fun NodeAgents(node: ExecutionNode, state: Workspace, openAgents: () -> Unit) {
    fun agents(count: Int) = if (count == 1) "1 agent peut" else "$count agents peuvent"
    val runners = node.agents.map { if (it.allNodes) "${it.name} (toutes les nodes)" else it.name }
    val exclusive =
        node.exclusiveAgents.mapNotNull { id ->
            node.agents.find { it.id == id }?.name ?: state.agents.find { it.id == id }?.name
        }
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(
            if (runners.isEmpty()) "Aucun agent ne peut exécuter ici."
            else "${agents(runners.size)} exécuter ici : ${runners.joinToString(", ")}",
            style = MaterialTheme.typography.bodyMedium,
        )
        Text(
            if (exclusive.isEmpty()) "Aucun agent ne peut la réserver en exclusivité."
            else
                "${agents(exclusive.size)} la réserver en exclusivité : ${exclusive.joinToString(", ")}",
            style = MaterialTheme.typography.bodyMedium,
        )
        TextButton(onClick = openAgents, contentPadding = PaddingValues(0.dp)) {
            Text("Gérer l’accès depuis les agents ›")
        }
    }
}

/**
 * The last step of adding a machine: nothing is chosen for the owner, except agents already allowed
 * on it or on every node. Those stay allowed.
 */
@Composable
private fun FirstAgents(
    node: ExecutionNode,
    state: Workspace,
    later: () -> Unit,
    finish: (List<String>) -> Unit,
) {
    var selected by remember(node.id) { mutableStateOf(emptySet<String>()) }
    DetailSheet("Qui peut utiliser « ${node.name} » ?", later) {
        Text(
            "Les agents choisis pourront exécuter leurs conversations sur cette machine. Un tag ou une capacité ne donne jamais d’accès à lui seul.",
            style = MaterialTheme.typography.bodyMedium,
        )
        val granted = node.grantedAgents(state.agents)
        state.agents.forEach { agent ->
            // The server refuses an agent on a node of another execution backend.
            val mismatch = !node.accepts(agent)
            val everywhere = !mismatch && agent.access.nodes == null
            val already = !mismatch && !everywhere && agent.id in granted
            val locked = everywhere || already
            val checked = locked || (!mismatch && agent.id in selected)
            Row(
                Modifier.fillMaxWidth().heightIn(min = 48.dp).toggleable(
                    value = checked,
                    enabled = !locked && !mismatch && !state.busy,
                    role = androidx.compose.ui.semantics.Role.Checkbox,
                ) {
                    selected = if (it) selected + agent.id else selected - agent.id
                },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Checkbox(checked, null, enabled = !locked && !mismatch)
                Spacer(Modifier.width(8.dp))
                Column(Modifier.weight(1f)) {
                    Text(agent.name)
                    val reason =
                        when {
                            mismatch ->
                                if (agent.access.backend == NATIVE_BACKEND)
                                    "Agent en exécution native : réservé aux nodes natives"
                                else "Agent en VM : réservé aux nodes VM"
                            everywhere -> "Autorisé sur toutes les nodes"
                            already -> "Déjà autorisé sur cette machine"
                            else -> null
                        }
                    reason?.let {
                        Text(
                            it,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        }
        Text(
            "Vous pourrez modifier cela plus tard sur la fiche de chaque agent.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        Row(
            Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
        ) {
            TextButton(onClick = later, enabled = !state.busy) { Text("Plus tard") }
            Button(
                onClick = {
                    finish(
                        state.agents.filter { it.id in selected && node.accepts(it) }.map { it.id }
                    )
                },
                enabled = !state.busy,
            ) {
                Text("Terminer")
            }
        }
    }
}

@Composable
internal fun NodeEditor(
    node: ExecutionNode,
    state: Workspace,
    nativeCallers: List<NativeCaller>,
    dismiss: () -> Unit,
    save: (NodeConfiguration) -> Unit,
) {
    var name by remember(node.id) { mutableStateOf(node.name) }
    var tags by remember(node.id) { mutableStateOf(node.tags.joinToString(", ")) }
    var accepting by remember(node.id) { mutableStateOf(node.accepting) }
    var nativeEnabled by remember(node.id) { mutableStateOf(node.nativeEnabled) }
    var nativeAccounts by remember(node.id) { mutableStateOf(node.nativeAccounts.toSet()) }
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
                    nativeEnabled =
                        if (node.capabilities.backend == NATIVE_BACKEND) nativeEnabled else null,
                    nativeAccounts =
                        if (node.capabilities.backend == NATIVE_BACKEND) nativeAccounts.toList()
                        else null,
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
            Field(
                "Slots d’exécution",
                slots,
                { slots = it },
                enabled = !state.busy && node.capabilities.backend != "native",
            )
            Field("Budget CPU partagé", cpu, { cpu = it }, enabled = !state.busy)
            Field("Budget RAM partagé (Gio)", memory, { memory = it }, enabled = !state.busy)
            Field("Budget disque partagé (Gio)", disk, { disk = it }, enabled = !state.busy)
            if (node.capabilities.backend == NATIVE_BACKEND)
                Text(
                    "Une node native exécute une conversation à la fois.",
                    style = MaterialTheme.typography.bodySmall,
                )
            Text(
                "Détecté sur cette machine : ${node.capabilities.cpu} CPU · ${formatMiB(node.capabilities.memoryMiB)} RAM · ${formatMiB(node.capabilities.diskMiB)} disque.",
                style = MaterialTheme.typography.bodySmall,
            )
        }
        if (node.capabilities.backend == "native")
            Panel {
                Text("Exécution native", style = MaterialTheme.typography.titleSmall)
                Text(
                    "Seuls les agents natifs et les personnes sélectionnées peuvent travailler ici. Retirer une autorisation arrête les travaux concernés à l’expiration de leur bail."
                )
                Toggle("Activer l’exécution native", nativeEnabled) { nativeEnabled = it }
                if (nativeCallers.isNotEmpty())
                    Text("Personnes de confiance", style = MaterialTheme.typography.labelLarge)
                if (nativeCallers.isEmpty())
                    Text(
                        "Aucune personne disponible. Actualisez la connexion de l’installation avant d’accorder l’accès.",
                        style = MaterialTheme.typography.bodySmall,
                    )
                nativeCallers.forEach { person ->
                    Toggle(person.email, person.id in nativeAccounts) { selected ->
                        nativeAccounts =
                            if (selected) nativeAccounts + person.id else nativeAccounts - person.id
                    }
                }
            }
        if (node.capabilities.backend != NATIVE_BACKEND && !node.exclusiveReservations)
            Text(
                "Mettez à jour le runtime de cette machine pour activer les réservations exclusives.",
                style = MaterialTheme.typography.bodySmall,
            )
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
                    // Edited on the web only; keep it rather than resetting it to the default.
                    memoryCacheMiB = policy.memoryCacheMiB,
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
