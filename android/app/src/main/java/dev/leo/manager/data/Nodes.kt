package dev.leo.manager.data

import kotlinx.serialization.Serializable

const val LOCAL_NODE_ID = "00000000-0000-4000-8000-000000000002"

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
data class NodeBackupSettings(val destination: String = "master", val intervalSeconds: Long = 60, val retention: Int = 3, val budgetMiB: Long = 102400, val disconnectTimeoutSeconds: Long = 60, val shutdownTimeoutSeconds: Long = 300, val maxCapacityWaitSeconds: Long = 3600)
