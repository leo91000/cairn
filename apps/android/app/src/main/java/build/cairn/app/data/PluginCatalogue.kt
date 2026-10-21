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
    val instanceReads: Boolean = false,
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

private suspend fun CairnApi.completePluginCatalogue(): PluginCatalog {
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
