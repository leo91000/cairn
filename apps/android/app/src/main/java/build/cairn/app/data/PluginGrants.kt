package build.cairn.app.data

import build.cairn.contracts.AgentPluginGrants
import build.cairn.contracts.PluginContributionGrant
import build.cairn.contracts.PluginGrantAccess
import build.cairn.contracts.PluginInstanceGrant
import build.cairn.contracts.PluginResourceType
import kotlinx.serialization.Serializable

@Serializable
data class PluginGrantDraft(
    val grants: List<PluginInstanceGrant> = emptyList(),
    val runCredentialInstances: List<String> = emptyList(),
) {
    fun without(instance: String) =
        copy(
            grants = grants.filter { it.instanceId != instance },
            runCredentialInstances = runCredentialInstances - instance,
        )

    fun description(instance: String): String =
        when (val access = grants.find { it.instanceId == instance }?.access) {
            is PluginGrantAccess.A -> "Accès complet, y compris les contributions futures"
            is PluginGrantAccess.B ->
                access.contributions.joinToString(" ; ") { item ->
                    "${item.kind} ${item.name} : " +
                        if (item.resources.isEmpty()) "sans restriction"
                        else
                            item.resources.joinToString {
                                "${it.resourceType} ${it.id} et descendants"
                            }
                }
            null -> "Aucun accès"
        }

    fun key(): String =
        wireJson.encodeToString(
            serializer(),
            copy(
                grants =
                    grants
                        .sortedBy { it.instanceId }
                        .map { grant ->
                            grant.copy(
                                access =
                                    when (val access = grant.access) {
                                        is PluginGrantAccess.A -> access
                                        is PluginGrantAccess.B ->
                                            PluginGrantAccess.B(
                                                access.contributions
                                                    .sortedBy { it.kind + ":" + it.name }
                                                    .map {
                                                        it.copy(
                                                            resources =
                                                                it.resources.sortedWith(
                                                                    compareBy(
                                                                        { it.resourceType },
                                                                        { it.id },
                                                                    )
                                                                )
                                                        )
                                                    }
                                            )
                                    }
                            )
                        },
                runCredentialInstances = runCredentialInstances.sorted(),
            ),
        )

    fun chooseSupplier(instances: List<String>, supplier: String?): PluginGrantDraft =
        copy(
            grants =
                grants.mapNotNull { grant ->
                    val access = grant.access
                    if (
                        grant.instanceId !in instances ||
                            grant.instanceId == supplier ||
                            access is PluginGrantAccess.A
                    )
                        grant
                    else {
                        val contributions =
                            (access as PluginGrantAccess.B).contributions.filter {
                                it.kind != "runCredentials"
                            }
                        if (contributions.isEmpty()) null
                        else grant.copy(access = PluginGrantAccess.B(contributions))
                    }
                },
            runCredentialInstances =
                runCredentialInstances.filter { it !in instances } + listOfNotNull(supplier),
        )
}

fun AgentPluginGrants.draft() = PluginGrantDraft(grants, runCredentialInstances)

data class GrantContribution(
    val grant: PluginContributionGrant,
    val description: String,
    val resourceTypes: List<PluginResourceType>,
)

fun PluginManifest.grantContributions(): List<GrantContribution> = buildList {
    fun add(
        kind: String,
        name: String = "",
        description: String,
        fieldTypes: List<String> = emptyList(),
    ) {
        val selectableTypes = resources.filter { selectable ->
            fieldTypes.any { fieldType ->
                var current: String? = fieldType
                val visited = mutableSetOf<String>()
                while (current != null && visited.add(current)) {
                    if (current == selectable.name) return@any true
                    current = resources.find { it.name == current }?.parentType
                }
                false
            }
        }
        add(
            GrantContribution(
                PluginContributionGrant(kind, name, emptyList()),
                description,
                selectableTypes,
            )
        )
    }
    tools.forEach {
        add("tool", it.name, it.description, it.resourceFields.map { field -> field.resourceType })
    }
    commands.forEach {
        add(
            "command",
            it.name,
            it.description,
            it.arguments.mapNotNull { argument -> argument.resourceType },
        )
    }
    hooks.forEach {
        add("hook", it.name, it.description, it.resourceFields.map { field -> field.resourceType })
    }
    if (runCredentials != null)
        add(
            "runCredentials",
            description = "Identifiants de run : accès direct avec ce compte de service",
        )
    if (instructions.isNotBlank())
        add("instructions", description = "Instructions ajoutées au prompt")
    conditions.forEach { add("condition", it.name, it.description) }
    projectSources.forEach { add("projectSource", it.name, it.title) }
    skillSources.forEach { add("skillSource", it.name, it.title) }
}

fun pluginGrantReview(
    before: PluginGrantDraft,
    after: PluginGrantDraft,
    catalog: PluginCatalog?,
): List<ReviewLine> = buildList {
    if (before.key() == after.key()) return@buildList
    (before.grants + after.grants)
        .map { it.instanceId }
        .distinct()
        .forEach { id ->
            if (before.description(id) != after.description(id)) {
                val name = catalog?.connections?.find { it.id == id }?.name ?: id
                add(
                    ReviewLine(
                        ReviewKind.Power,
                        "$name : ${after.description(id)}. Effectif immédiatement, y compris dans les conversations en cours.",
                    )
                )
            }
        }
    if (before.runCredentialInstances.toSet() != after.runCredentialInstances.toSet()) {
        val names =
            after.runCredentialInstances.map { id ->
                catalog?.connections?.find { it.id == id }?.name ?: id
            }
        add(
            ReviewLine(
                ReviewKind.Power,
                "Fournisseurs d’identifiants de run : ${names.joinToString().ifBlank { "Aucun" }}. Les identifiants rendent vaines les restrictions de ressource des outils.",
            )
        )
    }
}
