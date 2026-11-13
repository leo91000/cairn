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

typealias ChatExecution = build.cairn.contracts.ChatExecution

@Serializable
data class RunStorage(
    val mode: String = "",
    val localBytes: Long? = null,
    val dirtyBytes: Long? = null,
    val dirtySince: Long? = null,
    val waitingFor: String? = null,
)
