package build.cairn.app.data

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

typealias NodeAgent = build.cairn.contracts.NodeAgent

typealias NodeResources = build.cairn.contracts.NodeResources

typealias NodeCapabilities = build.cairn.contracts.NodeCapabilities

typealias ExecutionNode = build.cairn.contracts.ExecutionNode

typealias NodeDiskAccounting = build.cairn.contracts.NodeDiskAccounting

typealias NodeUsage = build.cairn.contracts.NodeUsage

typealias StaleDisks = build.cairn.contracts.StaleDisks

typealias NodeEnrollment = build.cairn.contracts.NodeEnrollment

typealias NativeCaller = build.cairn.contracts.NativeCaller

typealias NodeRevocation = build.cairn.contracts.NodeRevocation

typealias NodeConfiguration = build.cairn.contracts.NodeConfiguration

typealias NodeStoragePolicy = build.cairn.contracts.NodeStoragePolicy

const val LOCAL_NODE_ID = "00000000-0000-4000-8000-000000000002"

fun nativeModeLabel(mode: String) =
    if (mode == "administrator") "Natif administrateur" else "Natif protégé"

/** The execution backend of native nodes and agents; the other one is "firecracker" (a VM). */
const val NATIVE_BACKEND = "native"

/** The default execution backend: a Firecracker VM. */
const val VM_BACKEND = "firecracker"

/** Whether the node runs on the agent's execution backend: the server refuses any other grant. */
fun ExecutionNode.accepts(agent: Agent) =
    agent.access.backend == "auto" ||
        (capabilities.backend == NATIVE_BACKEND) == (agent.access.backend == NATIVE_BACKEND)

/**
 * Agents already allowed to run on this machine without being allowed everywhere, from the node
 * itself or from each agent's page. They are shown locked: granting more agents never revokes them.
 */
fun ExecutionNode.grantedAgents(agents: List<Agent> = emptyList()): Set<String> =
    if (nativeModeRequest?.mode == "administrator") nativeReviewedAgents.toSet()
    else
        this.agents.filter { !it.allNodes }.map { it.id }.toSet() +
            agents.filter { it.access.nodes?.contains(id) == true }.map { it.id }

/** Grant only newly chosen agents, preserving concurrent grants. */
suspend fun grantNodeAgents(
    chosen: List<String>,
    granted: Set<String>,
    confirmNativeAdministration: Boolean = false,
    send: suspend (JsonObject) -> Unit,
) {
    val added = chosen.distinct().filter { it !in granted }
    if (added.isEmpty()) return
    send(
        buildJsonObject {
            put("agentIds", buildJsonArray { added.forEach { add(JsonPrimitive(it)) } })
            put("add", true)
            if (confirmNativeAdministration) put("confirmNativeAdministration", true)
        }
    )
}

typealias ExclusiveReservation = build.cairn.boundary.ExclusiveReservation

fun exclusiveReservationState(state: String): String =
    when (state) {
        "queued" -> "En attente"
        "acquiring" -> "Suspension des autres tâches"
        "acquired" -> "Exclusivité acquise"
        "releasing" -> "Reprise des autres tâches"
        "released" -> "Libérée"
        "failed" -> "Échec de la réservation"
        "expired" -> "Expirée"
        else -> state
    }

/** Disk kept on a node that no conversation needs there any more. */
@Serializable data class StaleDiskCleanup(val freedMiB: Long = 0, val failed: Int = 0)

@Serializable
data class NodeAlert(
    val id: String,
    val chatId: String = "",
    val kind: String = "",
    val title: String = "",
    val body: String = "",
    val createdAt: Long = 0,
)

/** The app speaks French; the master's English text remains the fallback for unknown kinds. */
fun NodeAlert.localized(): Pair<String, String> =
    when (kind) {
        "waiting" ->
            "Conversation en attente de sa node" to
                "Sa machine est indisponible et aucune autre ne peut la reprendre pour l’instant. Elle repartira dès que possible."
        "resumed" ->
            "Conversation reprise sur une autre node" to
                "Sa machine est devenue indisponible ; elle a repris depuis son dernier point de reprise. Des fichiers récents peuvent manquer."
        "move-failed" ->
            "Échec du déplacement" to
                "Le transfert de l’environnement a échoué. Le disque d’origine est conservé."
        "backup-failed" ->
            "Synchronisation en échec" to
                "Le disque n’a pas pu être synchronisé avec S3. Ouvrez la conversation pour le détail."
        else -> title to body
    }

/** Reply to a revocation. */
typealias NodeBackup = build.cairn.boundary.NodeBackup

@Serializable
data class NodeSyncSettings(
    val intervalSeconds: Long = 60,
    val budgetMiB: Long = 102400,
    val disconnectTimeoutSeconds: Long = 60,
    val shutdownTimeoutSeconds: Long = 300,
    val maxCapacityWaitSeconds: Long = 3600,
    val s3Configured: Boolean? = null,
)

fun formatMiB(value: Long): String {
    if (value < 1024) return "$value Mio"
    val gib = value / 1024.0
    return if (value % 1024 == 0L) "${value / 1024} Gio"
    else "${"%.1f".format(java.util.Locale.FRANCE, gib)} Gio"
}

fun relativeAge(since: Long, now: Long = System.currentTimeMillis()): String {
    val seconds = ((now - since) / 1000).coerceAtLeast(0)
    if (seconds < 60) return "il y a moins d’une minute"
    val minutes = seconds / 60
    if (minutes < 60) return "il y a $minutes min"
    val hours = minutes / 60
    if (hours < 48) return "il y a $hours h"
    return "il y a ${hours / 24} jours"
}

enum class NodeHealthState {
    READY,
    FULL,
    BUSY,
    RESERVED,
    PAUSED,
    UPDATING,
    SETUP,
    ATTENTION,
    OFFLINE,
    REVOKED,
}

data class NodeHealth(val state: NodeHealthState, val label: String, val detail: String)

/** Whether a node can take work, as one state and a short sentence; the first blocker wins. */
fun nodeHealth(node: ExecutionNode, now: Long = System.currentTimeMillis()): NodeHealth {
    if (node.revoked) return NodeHealth(NodeHealthState.REVOKED, "Révoquée", "N’a plus accès")
    if (!node.local && node.status == "offline")
        return NodeHealth(
            NodeHealthState.OFFLINE,
            "Hors ligne",
            node.lastSeen?.let { "Dernier contact ${relativeAge(it, now)}" } ?: "Jamais connectée",
        )
    val native = node.capabilities.backend == NATIVE_BACKEND
    if (!native && !node.capabilities.kvm)
        return NodeHealth(NodeHealthState.ATTENTION, "À vérifier", "KVM indisponible")
    if (native && !node.capabilities.ublk)
        return NodeHealth(NodeHealthState.ATTENTION, "À vérifier", "ublk indisponible")
    // A privilege change blocks placements until the supervisor confirms it.
    if (
        native && (node.nativeModeStatus == "failed" || node.nativeModeObserved?.status == "failed")
    )
        return NodeHealth(
            NodeHealthState.ATTENTION,
            "À vérifier",
            "Le changement de privilèges a échoué",
        )
    // Supervisors without the privilege protocol never confirm a mode: the status stays pending.
    if (native && node.capabilities.nativeModeProtocol != 1L)
        return NodeHealth(
            NodeHealthState.ATTENTION,
            "À vérifier",
            "Mettez à jour le superviseur pour confirmer les privilèges",
        )
    if (native && node.nativeModeStatus == "pending")
        return NodeHealth(
            NodeHealthState.UPDATING,
            "Mise à jour",
            "Changement de privilèges en cours",
        )
    if (node.maintenance != null && node.maintenanceError != null)
        return NodeHealth(NodeHealthState.ATTENTION, "À vérifier", "La mise à jour a échoué")
    if (node.maintenance != null)
        return NodeHealth(
            NodeHealthState.UPDATING,
            "Mise à jour",
            "Les nouveaux travaux attendent la mise à jour",
        )
    if (!node.executionReady)
        return NodeHealth(
            NodeHealthState.ATTENTION,
            "À vérifier",
            if (node.budgetError != null) "Le budget partagé n’a pas pu être appliqué"
            else "Le runtime n’est pas prêt",
        )
    if (native && node.nativeModeProposal != null)
        return NodeHealth(
            NodeHealthState.ATTENTION,
            "À vérifier",
            "Un agent demande ${nativeModeLabel(node.nativeModeProposal.mode)}",
        )
    if (native && (!node.nativeEnabled || node.nativeAccounts.isEmpty()))
        return NodeHealth(
            NodeHealthState.SETUP,
            "À configurer",
            "Activez l’exécution native et choisissez les personnes de confiance",
        )
    if (node.agents.isEmpty())
        return NodeHealth(
            NodeHealthState.SETUP,
            "À configurer",
            "Aucun agent ne peut encore utiliser cette machine",
        )
    if (!node.accepting)
        return NodeHealth(NodeHealthState.PAUSED, "En pause", "Nouveaux travaux suspendus")
    node.exclusiveReservation?.let {
        return NodeHealth(NodeHealthState.RESERVED, "Réservée", it.reason)
    }
    node.pressure?.let {
        return NodeHealth(
            NodeHealthState.BUSY,
            "Chargée",
            if (it == "memory") "RAM partagée sous pression"
            else "Espace disque partagé insuffisant",
        )
    }
    val slots = node.slots ?: 0
    val free = node.availableSlots
    if (slots > 0 && free <= 0)
        return NodeHealth(NodeHealthState.FULL, "Pleine", "Les $slots slots sont occupés")
    val running = node.occupiedSlots
    return NodeHealth(
        NodeHealthState.READY,
        "Prête",
        if (running > 0) "$running en cours · $free slots libres sur $slots"
        else "$free slots libres sur $slots",
    )
}

/** Why a node cannot take new work right now; empty when it can. */
fun nodeDiagnostics(node: ExecutionNode, now: Long = System.currentTimeMillis()): List<String> {
    if (node.revoked) return emptyList()
    val reasons = mutableListOf<String>()
    if (!node.local && node.status == "offline")
        reasons +=
            node.lastSeen?.let {
                "Aucun contact depuis ${relativeAge(it, now).removePrefix("il y a ")} : vérifiez que la machine est allumée, connectée et que le service ${if (node.capabilities.backend == NATIVE_BACKEND) "cairn-native-node" else "cairn-node"} tourne."
            } ?: "La machine ne s’est jamais connectée : lancez la commande d’installation dessus."
    if (node.capabilities.backend != "native" && !node.capabilities.kvm)
        reasons +=
            "KVM indisponible : activez la virtualisation dans le BIOS et chargez le module kvm."
    if (node.capabilities.backend == NATIVE_BACKEND && !node.capabilities.ublk)
        reasons +=
            "ublk indisponible : cette node a besoin d’un runtime natif à jour et du module noyau ublk_drv (/dev/ublk-control) pour monter les disques de conversation stockés sur S3."
    if (node.status != "offline" && !node.executionReady)
        reasons +=
            node.budgetError?.let { "Le budget partagé n’a pas pu être appliqué : $it" }
                ?: node.updateError?.let { "Le runtime n’est pas prêt : $it" }
                ?: "Le runtime n’est pas encore prêt ou ne correspond pas à la version approuvée par le master."
    if (node.maintenance != null)
        reasons += "Maintenance en cours : les nouveaux travaux attendent la fin de la mise à jour."
    if (node.pressure != null)
        reasons +=
            if (node.pressure == "memory")
                "RAM partagée sous pression : les nouveaux travaux attendent."
            else "Espace disque partagé insuffisant : les nouveaux travaux attendent."
    if (!node.accepting)
        reasons +=
            "Nouveaux travaux suspendus sur cette machine (Configurer → Accepter de nouveaux travaux)."
    if (
        node.capabilities.backend == "native" &&
            (!node.nativeEnabled || node.nativeAccounts.isEmpty())
    )
        reasons +=
            "Activez l’exécution native et choisissez les personnes de confiance dans Configurer."
    if (node.agents.isEmpty()) reasons += "Aucun agent ne peut encore utiliser cette machine."
    return reasons
}

typealias RunStorage = build.cairn.boundary.RunStorage

fun formatBytes(bytes: Long): String {
    if (bytes < 1024) return "$bytes o"
    if (bytes < 1048576) {
        val kib = bytes / 1024.0
        val amount =
            if (bytes % 1024L == 0L) kib.toLong().toString()
            else "%.1f".format(java.util.Locale.FRANCE, kib)
        return "$amount Kio"
    }
    val mib = bytes / 1048576.0
    val value = if (mib < 1024) mib else mib / 1024
    val amount =
        if (value % 1.0 == 0.0) value.toLong().toString()
        else "%.1f".format(java.util.Locale.FRANCE, value)
    return "$amount " + if (mib < 1024) "Mio" else "Gio"
}
