// Temporary client adapters for the unconverted chat/node/outcome domains.
package build.cairn.boundary

import kotlinx.serialization.Serializable

typealias ContextUsage = build.cairn.contracts.ContextUsage

@Serializable
data class TaskOutcome(
    val status: String,
    val reason: String = "",
    val evidence: List<String> = emptyList(),
    val reportedAt: Long = 0,
    val messageId: String? = null,
    /** Who must act next on blocked work: user (the default), requester or external. */
    val waitingOn: String? = null,
)

@Serializable data class OutcomeDismissal(val until: Long? = null)

typealias ChatExecution = build.cairn.contracts.ChatExecution

@Serializable
data class ExclusiveReservation(
    val id: String,
    val nodeId: String,
    val ownerRunId: String,
    val reason: String,
    val state: String,
    val createdAt: Long = 0,
    val expiresAt: Long? = null,
    val renewalPending: Boolean = false,
    val waitUntil: Long = 0,
    val pausedRunIds: List<String> = emptyList(),
    val minimumAvailableMemoryMiB: Long = 0,
    val error: String? = null,
)

@Serializable
data class NodeBackup(
    val id: String? = null,
    val capturedAt: Long? = null,
    val status: String = "",
    val error: String? = null,
    val uploadedBytes: Long? = null,
)

@Serializable
data class RunStorage(
    val mode: String = "",
    val backupDegraded: Boolean = false,
    val localBytes: Long? = null,
    val dirtyBytes: Long? = null,
    val dirtySince: Long? = null,
    val waitingFor: String? = null,
)
