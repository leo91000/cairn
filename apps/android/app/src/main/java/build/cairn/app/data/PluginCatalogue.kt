package build.cairn.app.data

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.decodeFromJsonElement

/** Presentation is deliberately separate from a manifest that configures or approves code. */
@Serializable
data class PluginPresentation(
    val name: String,
    val description: String,
    val version: String,
    val brand: PluginBrand? = null,
    val instances: PluginInstances? = null,
    val capabilities: List<String>,
    val needsWorker: Boolean,
    val delegates: Boolean,
    val agentAccess: Boolean,
    val contributions: PluginContributionCounts,
    val agentAccessDescription: String? = null,
)

@Serializable
data class PluginContributionCounts(
    val commands: Int,
    val tools: Int,
    val instructions: Boolean,
    val view: Boolean,
    val test: Boolean,
    val form: Boolean,
    val collections: Int,
    val operations: Int? = null,
)

@Serializable
data class PluginInstanceSummary(
    val instances: Int,
    val states: Map<String, Int>,
    val pendingRequests: Int,
    /** Configured distinct agents from instance and resource rules, including paused rules. */
    val authorizedAgents: Int?,
    val observedAt: Long? = null,
)

@Serializable
data class PluginCardEntry(
    val id: String,
    val builtin: Boolean,
    val status: String,
    val active: String?,
    val presentation: PluginPresentation? = null,
    val versionCount: Int,
    val summary: PluginInstanceSummary? = null,
) {
    val name
        get() = presentation?.name ?: id

    fun summary(): PluginSummary {
        if (status == "needs_approval" || status == "invalid")
            return PluginSummary(
                if (status == "invalid") PluginBadge("Invalide", PluginTone.ATTENTION)
                else PluginBadge("À approuver", PluginTone.WARNING),
                presentation?.let { "Version ${it.version}" } ?: "Vérifiez les fichiers du package",
                0,
            )
        val current = checkNotNull(summary)
        val online = (current.states["ready"] ?: 0) + (current.states["connected"] ?: 0)
        val noun =
            presentation?.instances?.let {
                if (current.instances == 1) it.singular else it.plural
            } ?: if (current.instances == 1) "instance" else "instances"
        val agents = current.authorizedAgents
        val granted =
            if (presentation?.agentAccess == true) {
                if (agents == null) " · Détails des accès indisponibles"
                else " · $agents agent${if (agents > 1) "s" else ""} avec un accès enregistré"
            } else ""
        val live =
            listOf("needs_review", "connecting", "disconnected", "paused").firstOrNull {
                (current.states[it] ?: 0) > 0
            } ?: current.states.entries.firstOrNull { it.value > 0 }?.key ?: "disconnected"
        return PluginSummary(
            when {
                status == "update" -> PluginBadge("Mise à jour à examiner", PluginTone.WARNING)
                current.instances == 0 -> PluginBadge("À configurer", PluginTone.MUTED)
                online > 0 ->
                    PluginBadge(
                        if (presentation?.needsWorker == false)
                            "$online prêt${if (online > 1) "s" else ""}"
                        else "$online en ligne",
                        PluginTone.OK,
                    )
                else -> pluginConnectionState(live != "paused", live)
            },
            if (current.instances == 0) "Version ${active.orEmpty()}"
            else "${current.instances} ${noun.lowercase()}$granted",
            current.pendingRequests,
        )
    }
}

@Serializable
data class PluginCards(
    val packageReads: Boolean = false,
    val agentReads: Boolean = false,
    val instanceReads: Boolean = false,
    val accessReads: Boolean = false,
    val setupDrafts: Boolean = false,
    val capabilities: List<PluginCapability> = emptyList(),
    val items: List<PluginCardEntry>,
    val nextCursor: String? = null,
)

private fun PluginCatalog.cards() =
    PluginCards(
        setupDrafts = setupDrafts,
        capabilities = capabilities,
        items =
            packages.map { entry ->
                val manifest = entry.manifest
                val mine = connections.filter { it.pluginId == entry.id }
                PluginCardEntry(
                    entry.id,
                    entry.builtin,
                    entry.status,
                    entry.active,
                    manifest?.let {
                        PluginPresentation(
                            it.name,
                            it.description,
                            it.version,
                            it.brand,
                            it.instances,
                            it.capabilities,
                            it.needsWorker,
                            it.delegates,
                            it.agentAccess != null,
                            PluginContributionCounts(
                                it.commands.size,
                                it.tools.size,
                                it.instructions.isNotEmpty(),
                                it.instanceView,
                                it.instanceTest,
                                it.instanceForm,
                                it.resources.size,
                            ),
                        )
                    },
                    entry.versions.size,
                    PluginInstanceSummary(
                        mine.size,
                        mine
                            .groupingBy { if (it.enabled) it.status.state else "paused" }
                            .eachCount(),
                        mine.sumOf { it.requests.size },
                        // Older hosts expose resource totals per agent, not per package.
                        if (resourceGrantCounts.orEmpty().values.any { it > 0 }) null
                        else mine.flatMap { it.agentIds }.distinct().size,
                    ),
                )
            },
    )

internal suspend fun CairnApi.completePluginCatalogue(): PluginCatalog {
    val value = get<JsonObject>("/plugins")
    check(listOf("packages", "connections", "capabilities").all { value[it] is JsonArray }) {
        "Le catalogue de plugins est incomplet. Actualisez-le."
    }
    return wireJson.decodeFromJsonElement(value)
}

internal suspend fun CairnApi.pluginCards(): PluginCards {
    val first =
        try {
            get<PluginCards>("/plugins/packages")
        } catch (e: ApiException) {
            if (e.status != 404) throw e
            return completePluginCatalogue().cards()
        }
    check(first.packageReads) { "Le catalogue de plugins est incomplet. Actualisez-le." }
    if (!first.instanceReads) return completePluginCatalogue().cards()
    val items =
        pluginManagementItems("/plugins/packages", PluginPage(first.items, first.nextCursor))
    check(
        items.all { item ->
            item.summary?.let { summary ->
                summary.instances >= 0 &&
                    summary.pendingRequests >= 0 &&
                    summary.authorizedAgents != null &&
                    summary.authorizedAgents >= 0 &&
                    summary.states.values.all { it >= 0 }
            } == true
        }
    ) {
        "Les résumés des plugins sont indisponibles. Actualisez le catalogue."
    }
    return first.copy(items = items, nextCursor = null)
}

internal data class PluginReviewRead(val entry: PluginEntry, val lazy: Boolean)

internal suspend fun CairnApi.pluginReview(id: String): PluginReviewRead {
    val root = "/plugins/packages/${segment(id)}"
    val detail =
        try {
            get<PluginEntry>(root)
        } catch (e: ApiException) {
            if (e.status != 404) throw e
            val entry =
                completePluginCatalogue().packages.find { it.id == id }
                    ?: error("Package de plugin introuvable.")
            return PluginReviewRead(entry, false)
        }
    check(detail.id == id) { "La configuration reçue ne correspond pas à ce package." }
    val versions = pluginManagementItems<PluginVersion>("$root/versions")
    check(
        versions.all {
            it.version.isNotEmpty() &&
                (it.error != null || (it.digest != null && it.approved != null))
        }
    ) {
        "La liste des versions est incomplète. Rouvrez la revue."
    }
    return PluginReviewRead(detail.copy(versions = versions), true)
}

/** One configuration manifest; review history is intentionally absent. */
@Serializable
internal data class PluginConfigurationEntry(
    val id: String,
    val builtin: Boolean,
    val status: String,
    val active: String?,
    val manifest: PluginManifest?,
    val instanceReads: Boolean = false,
    val accessReads: Boolean = false,
    val setupDrafts: Boolean = false,
    val capabilities: List<PluginCapability>? = null,
) {
    val name
        get() = manifest?.name ?: id
}

internal fun PluginEntry.configuration() =
    PluginConfigurationEntry(id, builtin, status, active, manifest)

/** Saved root and resource IDs remain distinct; neither asserts effective authorization. */
@Serializable
internal data class PluginInstanceAccess(
    val id: String,
    val pluginId: String,
    val pluginName: String,
    val name: String,
    val enabled: Boolean,
    val agentIds: List<String>,
    val resourceAgentIds: List<String>,
)

internal data class PluginConfiguration(
    val entry: PluginConfigurationEntry?,
    val capabilities: List<PluginCapability>,
    val setupDrafts: Boolean,
    val exactInstanceReads: Boolean,
    val connections: List<PluginConnection>,
    val access: List<PluginInstanceAccess>,
    val resourceAgentIds: List<String> = emptyList(),
) {
    fun hasPluginAccess(agentId: String) =
        agentId in resourceAgentIds ||
            access.any {
                agentId in it.agentIds || agentId in it.resourceAgentIds
            }

    fun grantsOf(agentId: String): List<String> =
        access
            .filter {
                agentId in it.agentIds || agentId in it.resourceAgentIds
            }
            .map {
                "${if (it.pluginId == entry?.id) entry?.name ?: it.pluginName else it.pluginName} · ${it.name}"
            }
}

private suspend fun CairnApi.legacyConfiguration(id: String): PluginConfiguration {
    val catalog = completePluginCatalogue()
    val entry = catalog.packages.find { it.id == id }
    return PluginConfiguration(
        entry?.configuration(),
        catalog.capabilities,
        catalog.setupDrafts,
        false,
        catalog.connections.filter { it.pluginId == id },
        catalog.connections.map {
            PluginInstanceAccess(
                it.id,
                it.pluginId,
                catalog.packages.find { entry -> entry.id == it.pluginId }?.name ?: it.pluginId,
                it.name,
                it.enabled,
                it.agentIds,
                emptyList(),
            )
        },
        catalog.resourceGrantCounts.orEmpty().filterValues { it > 0 }.keys.toList(),
    )
}

/** Discover support from one compact page; a removed package needs no complete catalogue. */
private suspend fun CairnApi.missingConfiguration(id: String): PluginConfiguration {
    val support =
        try {
            get<PluginCards>("/plugins/packages")
        } catch (e: ApiException) {
            if (e.status != 404) throw e
            return legacyConfiguration(id)
        }
    check(support.packageReads) { "Le catalogue de plugins est incomplet. Actualisez-le." }
    if (!support.instanceReads || !support.accessReads) return legacyConfiguration(id)
    return PluginConfiguration(
        null,
        support.capabilities,
        support.setupDrafts,
        true,
        emptyList(),
        emptyList(),
    )
}

internal suspend fun CairnApi.pluginConfiguration(id: String): PluginConfiguration {
    val root = "/plugins/packages/${segment(id)}"
    val detail =
        try {
            get<PluginConfigurationEntry>(root)
        } catch (e: ApiException) {
            if (e.status != 404) throw e
            return missingConfiguration(id)
        }
    check(detail.id == id) { "La configuration reçue ne correspond pas à ce package." }
    if (!detail.instanceReads || !detail.accessReads) return legacyConfiguration(id)
    val capabilities =
        checkNotNull(detail.capabilities) {
            "La configuration du plugin est incomplète. Actualisez-la."
        }
    val connections = pluginManagementItems<PluginConnection>("$root/connections")
    val access = pluginManagementItems<PluginInstanceAccess>("/plugins/instance-access")
    check(
        connections.all { it.pluginId == id } &&
            access.all { it.id.isNotEmpty() && it.pluginId.isNotEmpty() }
    ) {
        "Les accès enregistrés du plugin sont incomplets. Actualisez-les."
    }
    return PluginConfiguration(detail, capabilities, detail.setupDrafts, true, connections, access)
}

/**
 * Complete current root IDs for an older-host compatibility save. Never recreate a vanished
 * instance.
 */
internal suspend fun CairnApi.pluginInstance(
    plugin: String,
    instance: String,
    exact: Boolean,
): PluginConnection {
    val saved =
        if (exact) {
            val value =
                get<JsonObject>(
                    "/plugins/packages/${segment(plugin)}/connections/${segment(instance)}"
                )
            check(value["agentIds"] is JsonArray) {
                "Les accès enregistrés de l’instance sont incomplets. Rouvrez le plugin."
            }
            wireJson.decodeFromJsonElement<PluginConnection>(value)
        } else
            completePluginCatalogue().connections.find {
                it.id == instance && it.pluginId == plugin
            } ?: error("Cette instance a été supprimée. Rouvrez le plugin avant d’enregistrer.")
    check(saved.id == instance && saved.pluginId == plugin) {
        "L’instance reçue ne correspond pas au plugin. Rouvrez-le."
    }
    return saved
}
