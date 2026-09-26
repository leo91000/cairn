package dev.leo.manager.data

import kotlinx.serialization.Serializable

const val LOCAL_NODE_ID = "00000000-0000-4000-8000-000000000002"

/** The backend applies the same defaults when a conversation has not requested resources. */
val DEFAULT_RESOURCES = NodeResources(cpu = 2, memoryMiB = 4096, diskMiB = 32768)

@Serializable
data class NodeAgent(val id: String, val name: String = "", val allNodes: Boolean = false)

@Serializable
data class NodeResources(val cpu: Int = 1, val memoryMiB: Long = 128, val diskMiB: Long = 128)

@Serializable
data class NodeCapabilities(
    val cpu: Int = 1,
    val memoryMiB: Long = 128,
    val diskMiB: Long = 128,
    val os: String = "",
    val arch: String = "",
    val kvm: Boolean = false,
)

@Serializable
data class ExecutionNode(
    val id: String,
    val name: String,
    val local: Boolean = false,
    val accepting: Boolean = false,
    val revoked: Boolean = false,
    val status: String,
    val tags: List<String> = emptyList(),
    val capabilities: NodeCapabilities = NodeCapabilities(),
    val limits: NodeResources = NodeResources(),
    val available: NodeResources? = null,
    val reserved: NodeResources? = null,
    val executionReady: Boolean = false,
    val maintenance: String? = null,
    val imageDigest: String? = null,
    val updateError: String? = null,
    val runtimeId: String = "",
    val lastSeen: Long? = null,
    val systemTags: List<String> = emptyList(),
    val maintenanceError: String? = null,
    val agents: List<NodeAgent> = emptyList(),
)

@Serializable
data class NodeEnrollment(val code: String, val expiresAt: Long, val installCommand: String? = null)

@Serializable
data class NodeConfiguration(
    val name: String,
    val accepting: Boolean,
    val tags: List<String>,
    val limits: NodeResources,
)

@Serializable
data class NodeBackup(val id: String? = null, val capturedAt: Long? = null, val status: String = "", val error: String? = null, val uploadedBytes: Long? = null)

@Serializable
data class NodeBackupSettings(val destination: String = "master", val intervalSeconds: Long = 60, val retention: Int = 3, val budgetMiB: Long = 102400, val disconnectTimeoutSeconds: Long = 60, val shutdownTimeoutSeconds: Long = 300, val maxCapacityWaitSeconds: Long = 3600, val s3Configured: Boolean? = null)

fun formatMiB(value: Long): String {
    if (value < 1024) return "$value Mio"
    val gib = value / 1024.0
    return if (value % 1024 == 0L) "${value / 1024} Gio" else "${"%.1f".format(java.util.Locale.FRANCE, gib)} Gio"
}

fun formatResources(value: NodeResources) =
    "${value.cpu} CPU · ${formatMiB(value.memoryMiB)} RAM · ${formatMiB(value.diskMiB)} disque"

fun relativeAge(since: Long, now: Long = System.currentTimeMillis()): String {
    val seconds = ((now - since) / 1000).coerceAtLeast(0)
    if (seconds < 60) return "il y a moins d’une minute"
    val minutes = seconds / 60
    if (minutes < 60) return "il y a $minutes min"
    val hours = minutes / 60
    if (hours < 48) return "il y a $hours h"
    return "il y a ${hours / 24} jours"
}

/** Why a node cannot take new work right now; empty when it can. */
fun nodeDiagnostics(node: ExecutionNode, now: Long = System.currentTimeMillis()): List<String> {
    if (node.revoked) return emptyList()
    val reasons = mutableListOf<String>()
    if (!node.local && node.status == "offline")
        reasons += node.lastSeen?.let { "Aucun contact depuis ${relativeAge(it, now).removePrefix("il y a ")} : vérifiez que la machine est allumée, connectée et que le service leo-node tourne." }
            ?: "La machine ne s’est jamais connectée : lancez la commande d’installation dessus."
    if (!node.capabilities.kvm) reasons += "KVM indisponible : activez la virtualisation dans le BIOS et chargez le module kvm."
    if (node.status != "offline" && !node.executionReady)
        reasons += node.updateError?.let { "Le runtime n’est pas prêt : $it" } ?: "Le runtime n’est pas encore prêt ou ne correspond pas à la version approuvée par le master."
    if (node.maintenance != null) reasons += "Maintenance en cours : les nouveaux travaux attendent la fin de la mise à jour."
    if (!node.accepting) reasons += "Nouveaux travaux suspendus sur cette machine (Configurer → Accepter de nouveaux travaux)."
    if (node.agents.isEmpty()) reasons += "Aucun agent ne peut encore utiliser cette machine."
    return reasons
}
