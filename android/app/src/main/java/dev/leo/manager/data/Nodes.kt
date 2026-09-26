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
    val runtimeId: String = "",
    val lastSeen: Long? = null,
)

@Serializable
data class NodeEnrollment(val code: String, val expiresAt: Long)

@Serializable
data class NodeConfiguration(
    val name: String,
    val accepting: Boolean,
    val tags: List<String>,
    val limits: NodeResources,
)
