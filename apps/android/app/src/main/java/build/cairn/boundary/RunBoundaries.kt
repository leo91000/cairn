// Temporary client adapters for the unconverted chat/node/outcome domains.
package build.cairn.boundary

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject

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

@Serializable
data class ChatExecution(
    val messageId: String,
    val text: String = "",
    val recovery: Boolean = false,
)

@Serializable
data class ChatWakeup(
    val id: String,
    val messageId: String? = null,
    val reason: String,
    val seconds: Long,
    val createdAt: Long,
    val wakeAt: Long,
    val condition: JsonObject? = null,
    /**
     * A group's progress, in the order of its conditions: `{ observation, display, nextCheckAt,
     * lastError }` each. Kept as JSON, like [display].
     */
    val members: JsonArray? = null,
    val observation: JsonObject? = null,
    /** Plugin conditions: when Cairn checks again, and why its last check failed. */
    val nextCheckAt: Long? = null,
    val lastError: String? = null,
    /**
     * A plugin condition's words while it waits: `{ title, short, details[] }`. Kept as JSON so a
     * plugin's unexpected shape never hides the conversation.
     */
    val display: JsonObject? = null,
)

@Serializable
data class NodeResources(val cpu: Int = 1, val memoryMiB: Long = 128, val diskMiB: Long = 128)

@Serializable
data class NativeModeState(
    val mode: String = "protected",
    val generation: String? = null,
    val status: String = "",
    val error: String? = null,
)

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
    val localBytes: Long? = null,
    val dirtyBytes: Long? = null,
    val dirtySince: Long? = null,
    val waitingFor: String? = null,
)
