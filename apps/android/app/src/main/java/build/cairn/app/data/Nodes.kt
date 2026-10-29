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

/**
 * The server's heartbeat timeout; listing nodes refreshes the built-in runner within it when
 * reachable.
 */
private const val HEARTBEAT_TIMEOUT_MS = 60_000L

/**
 * The server's clock as seen in an inventory: the latest contact of a machine the server reports
 * online. Device clocks can drift from the server's, so staleness is judged against this.
 */
fun fleetClock(nodes: List<ExecutionNode>): Long? =
    nodes
        .filter { !it.local && !it.revoked && it.status == "online" }
        .mapNotNull { it.lastSeen }
        .maxOrNull()

/** The built-in runner's last contact, and when this device first saw that value. */
data class RunnerObservation(val lastSeen: Long, val observedAt: Long)

/** Keeps the first time each runner contact was seen across inventory refreshes. */
fun observeRunner(
    nodes: List<ExecutionNode>,
    previous: RunnerObservation?,
    now: Long = System.currentTimeMillis(),
): RunnerObservation? {
    val seen = nodes.firstOrNull { it.local && !it.revoked }?.lastSeen ?: return null
    return if (previous?.lastSeen == seen) previous else RunnerObservation(seen, now)
}

/** How long the built-in runner's contact is known to lag server time, if it is. */
private fun runnerLag(node: ExecutionNode, clock: Long?): Long? =
    if (clock != null && node.lastSeen != null) clock - node.lastSeen else null

/**
 * The built-in runner always reports "local". Listing refreshes its contact (every refresh, 10 s)
 * only when it answers with usable health, so it is stale when it lags the server clock by the
 * heartbeat timeout, or when its contact has not changed for that long on this device. Neither
 * depends on the device clock's offset; a runner already stale when the screen opens is found after
 * a minute at most. The failure may be no answer, a mismatched version or missing capabilities.
 */
fun runnerStale(
    node: ExecutionNode,
    clock: Long?,
    runner: RunnerObservation? = null,
    now: Long = System.currentTimeMillis(),
): Boolean {
    if (!node.local || node.revoked || node.lastSeen == null) return false
    val lag = runnerLag(node, clock)
    if (lag != null && lag >= HEARTBEAT_TIMEOUT_MS) return true
    return runner?.lastSeen == node.lastSeen && now - runner.observedAt >= HEARTBEAT_TIMEOUT_MS
}

/**
 * The stale runner's age when a server reference shows it; otherwise only its lower bound is known.
 */
private fun runnerSilence(node: ExecutionNode, clock: Long?): String? {
    val lag = runnerLag(node, clock) ?: return null
    return if (lag >= HEARTBEAT_TIMEOUT_MS) relativeAge(node.lastSeen!!, clock!!) else null
}

/**
 * Whether a node can take work, as one state and a short sentence; the first blocker wins. Online
 * follows the server's 60-second heartbeat timeout; placement wants a heartbeat within 30 seconds,
 * so a machine that just stopped answering can read ready for up to half a minute.
 */
fun nodeHealth(
    node: ExecutionNode,
    now: Long = System.currentTimeMillis(),
    clock: Long? = null,
    runner: RunnerObservation? = null,
): NodeHealth {
    if (node.revoked) return NodeHealth(NodeHealthState.REVOKED, "Révoquée", "N’a plus accès")
    // Ages use the server clock when the fleet shows it, so device clock drift does not shift them.
    val reference = clock ?: now
    if (!node.local && node.status == "offline")
        return NodeHealth(
            NodeHealthState.OFFLINE,
            "Hors ligne",
            node.lastSeen?.let { "Dernier contact ${relativeAge(it, maxOf(reference, it))}" }
                ?: "Jamais connectée",
        )
    if (runnerStale(node, clock, runner, now))
        return NodeHealth(
            NodeHealthState.ATTENTION,
            "À vérifier",
            runnerSilence(node, clock)?.let { "Dernier rapport exploitable $it" }
                ?: "Aucun rapport exploitable depuis plus d’une minute",
        )
    val native = node.capabilities.backend == NATIVE_BACKEND
    if (!native && !node.capabilities.kvm)
        return NodeHealth(NodeHealthState.ATTENTION, "À vérifier", "KVM indisponible")
    if (native && !node.capabilities.ublk)
        return NodeHealth(NodeHealthState.ATTENTION, "À vérifier", "ublk indisponible")
    // A privilege change blocks placements until the supervisor confirms it. The server reports
    // a failure only for the current request; an older failed attempt is still pending.
    if (native && node.nativeModeStatus == "failed")
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
        return NodeHealth(
            NodeHealthState.ATTENTION,
            "À vérifier",
            "La préparation de la mise à jour a échoué",
        )
    if (node.maintenance != null)
        return NodeHealth(
            NodeHealthState.UPDATING,
            "Mise à jour",
            "Les nouveaux travaux attendent la mise à jour",
        )
    // Without a reported error, readiness is usually on its way, as after a budget change until
    // the next heartbeat: it waits rather than alarms. The inventory cannot tell a lasting cause
    // (image, pool, data root) from that wait; the diagnostics list the possible causes.
    if (!node.executionReady)
        return when {
            node.budgetError != null ->
                NodeHealth(
                    NodeHealthState.ATTENTION,
                    "À vérifier",
                    "Le budget partagé n’a pas pu être appliqué",
                )
            node.updateError != null ->
                NodeHealth(
                    NodeHealthState.ATTENTION,
                    "À vérifier",
                    "La mise à jour a échoué",
                )
            else ->
                NodeHealth(
                    NodeHealthState.UPDATING,
                    "Pas prête",
                    "Le runtime n’est pas encore prêt",
                )
        }
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
fun nodeDiagnostics(
    node: ExecutionNode,
    now: Long = System.currentTimeMillis(),
    clock: Long? = null,
    runner: RunnerObservation? = null,
): List<String> {
    if (node.revoked) return emptyList()
    val reasons = mutableListOf<String>()
    val reference = clock ?: now
    if (runnerStale(node, clock, runner, now))
        reasons +=
            "Le runner du master n’envoie plus de rapport exploitable ${runnerSilence(node, clock)?.let { "depuis ${it.removePrefix("il y a ")}" } ?: "depuis plus d’une minute"} : vérifiez qu’il tourne, à la même version que le serveur."
    if (!node.local && node.status == "offline")
        reasons +=
            node.lastSeen?.let {
                "Aucun contact depuis ${relativeAge(it, maxOf(reference, it)).removePrefix("il y a ")} : vérifiez que la machine est allumée, connectée et que le service ${if (node.capabilities.backend == NATIVE_BACKEND) "cairn-native-node" else "cairn-node"} tourne."
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
