package build.cairn.app.data

import kotlinx.serialization.Serializable

/** Saved metadata describes configured access, never effective execution permission. */
data class AgentPluginPackage(
    val id: String,
    val status: String,
    val active: String?,
    val name: String,
    val instances: PluginInstances?,
    val agentAccess: Boolean,
    val agentAccessDescription: String,
    val operations: Int,
)

@Serializable
data class AgentPluginInstance(
    val id: String,
    val pluginId: String,
    val name: String,
    val enabled: Boolean,
    val agentId: String,
    val agentIds: List<String>,
    val status: PluginStatus,
)

data class AgentPluginDirectory(
    val packages: List<AgentPluginPackage> = emptyList(),
    val connections: List<AgentPluginInstance> = emptyList(),
    val resourceGrantCounts: Map<String, Int>? = null,
    /** Full manifests survive only in the explicit older-host adapter. */
    val legacyPackages: List<PluginEntry>? = null,
)

fun PluginEntry.agentPackage() =
    AgentPluginPackage(
        id,
        status,
        active,
        name,
        manifest?.instances,
        manifest?.agentAccess != null,
        manifest?.agentAccess?.description.orEmpty(),
        manifest?.operations?.size ?: 0,
    )

fun PluginConnection.agentInstance() =
    AgentPluginInstance(
        id,
        pluginId,
        name,
        enabled,
        agentId,
        agentIds,
        status,
    )

fun PluginCatalog.agentDirectory() =
    AgentPluginDirectory(
        packages.map { it.agentPackage() },
        connections.map { it.agentInstance() },
        resourceGrantCounts,
        packages,
    )

@Serializable internal data class AgentResourceChoice(val id: String, val name: String)

@Serializable private data class AgentGrantCount(val id: String, val count: Int)

internal suspend fun CairnApi.agentPluginDirectory(): AgentPluginDirectory {
    val first =
        try {
            get<PluginCards>("/plugins/packages")
        } catch (e: ApiException) {
            if (e.status != 404) throw e
            return completePluginCatalogue().agentDirectory()
        }
    check(first.packageReads) { "L’annuaire des plugins est incomplet." }
    if (!first.agentReads) return completePluginCatalogue().agentDirectory()
    val cards =
        pluginManagementItems("/plugins/packages", PluginPage(first.items, first.nextCursor))
    val connections = pluginManagementItems<AgentPluginInstance>("/plugins/instance-access")
    val counts = pluginManagementItems<AgentGrantCount>("/plugins/agent-grant-counts")
    check(
        cards.all {
            it.presentation == null || (it.presentation.contributions.operations ?: -1) >= 0
        } && counts.all { it.id.isNotBlank() && it.count >= 0 }
    ) {
        "Les accès enregistrés des plugins n’ont pas pu être chargés entièrement."
    }
    return AgentPluginDirectory(
        cards.map { entry ->
            val view = entry.presentation
            AgentPluginPackage(
                entry.id,
                entry.status,
                entry.active,
                view?.name ?: entry.id,
                view?.instances,
                view?.agentAccess == true,
                view?.agentAccessDescription.orEmpty(),
                view?.contributions?.operations ?: 0,
            )
        },
        connections,
        counts.associate { it.id to it.count },
    )
}

internal suspend fun CairnApi.agentPluginManifest(
    directory: AgentPluginDirectory,
    id: String,
): PluginManifest {
    val entry =
        if (directory.legacyPackages != null) {
            directory.legacyPackages.find { it.id == id }?.configuration()
        } else get<PluginConfigurationEntry>("/plugins/packages/${segment(id)}")
    check(entry?.id == id && entry.active != null && entry.manifest?.id == id) {
        "Le schéma du plugin sélectionné est indisponible. Actualisez ses accès."
    }
    return checkNotNull(entry.manifest)
}
