package build.cairn.app.data

import build.cairn.contracts.AgentPluginGrants
import build.cairn.contracts.PluginContributionGrant
import build.cairn.contracts.PluginGrantAccess
import build.cairn.contracts.PluginInstanceGrant
import build.cairn.contracts.PluginResourceRef
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

    fun withoutContribution(instance: String, kind: String, name: String): PluginGrantDraft =
        copy(
            grants =
                grants.mapNotNull { grant ->
                    if (grant.instanceId != instance || grant.access !is PluginGrantAccess.B)
                        return@mapNotNull grant

                    val contributions =
                        grant.access.contributions.filterNot {
                            it.kind == kind && it.name == name
                        }
                    if (contributions.isEmpty()) null
                    else grant.copy(access = PluginGrantAccess.B(contributions))
                },
            runCredentialInstances =
                if (kind == "runCredentials") runCredentialInstances - instance
                else runCredentialInstances,
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

private fun grantValueKey(grant: PluginInstanceGrant?): String? = grant?.let {
    PluginGrantDraft(listOf(it)).key()
}

private fun <T> mergeSet(
    base: List<T>,
    mine: List<T>,
    theirs: List<T>,
    identity: (T) -> String,
): List<T> {
    val items = (base + mine + theirs).associateBy(identity)
    val baseIds = base.map(identity).toSet()
    val mineIds = mine.map(identity).toSet()
    val theirsIds = theirs.map(identity).toSet()
    val identities = (base + mine + theirs).map(identity).distinct().sorted()

    return identities.mapNotNull { id ->
        val keep = if ((id in mineIds) == (id in baseIds)) id in theirsIds else id in mineIds
        if (keep) items[id] else null
    }
}

private fun contributionKey(contribution: PluginContributionGrant) =
    "${contribution.kind}:${contribution.name}"

private fun contributionValueKey(contribution: PluginContributionGrant) =
    wireJson.encodeToString(
        PluginContributionGrant.serializer(),
        contribution.copy(
            resources = contribution.resources.sortedWith(compareBy({ it.resourceType }, { it.id }))
        ),
    )

private fun mergeContributions(
    base: List<PluginContributionGrant>,
    mine: List<PluginContributionGrant>,
    theirs: List<PluginContributionGrant>,
): List<PluginContributionGrant> {
    val baseByKey = base.associateBy(::contributionKey)
    val mineByKey = mine.associateBy(::contributionKey)
    val theirsByKey = theirs.associateBy(::contributionKey)
    val keys = (base + mine + theirs).map(::contributionKey).distinct().sorted()

    return keys.mapNotNull { key ->
        val baseItem = baseByKey[key]
        val mineItem = mineByKey[key]
        val theirsItem = theirsByKey[key]
        val baseValue = baseItem?.let(::contributionValueKey)
        val mineValue = mineItem?.let(::contributionValueKey)
        val theirsValue = theirsItem?.let(::contributionValueKey)

        when {
            mineValue == baseValue -> theirsItem
            theirsValue == baseValue || mineValue == theirsValue -> mineItem
            // Removing a contribution is an explicit revocation. Do not let a
            // concurrent edit to its roots restore it during rebase.
            baseItem != null && (mineItem == null || theirsItem == null) -> null
            else -> {
                val template = mineItem ?: theirsItem ?: baseItem ?: return@mapNotNull null
                val resources =
                    mergeSet(
                        baseItem?.resources.orEmpty(),
                        mineItem?.resources.orEmpty(),
                        theirsItem?.resources.orEmpty(),
                    ) { resource ->
                        "${resource.resourceType}:${resource.id}"
                    }
                when {
                    resources.size > 100 -> mineItem ?: theirsItem
                    baseItem?.resources?.isNotEmpty() == true &&
                        mineItem?.resources?.isNotEmpty() == true &&
                        theirsItem?.resources?.isNotEmpty() == true &&
                        resources.isEmpty() -> mineItem
                    else -> template.copy(resources = resources)
                }
            }
        }
    }
}

private fun mergeGrant(
    base: PluginInstanceGrant?,
    mine: PluginInstanceGrant?,
    theirs: PluginInstanceGrant?,
    instanceId: String,
): PluginInstanceGrant? {
    val baseValue = grantValueKey(base)
    val mineValue = grantValueKey(mine)
    val theirsValue = grantValueKey(theirs)
    if (mineValue == baseValue) return theirs
    if (theirsValue == baseValue || mineValue == theirsValue) return mine

    // Preserve a revocation made on either side instead of rebuilding access from
    // the surviving side's selected contributions.
    if (base != null && (mine == null || theirs == null)) return null

    // Independent selected contributions merge; mode changes remain the local choice and
    // appear in the sensitive review against the latest server snapshot.
    if (listOf(base, mine, theirs).all { it == null || it.access is PluginGrantAccess.B }) {
        val contributions =
            mergeContributions(
                (base?.access as? PluginGrantAccess.B)?.contributions.orEmpty(),
                (mine?.access as? PluginGrantAccess.B)?.contributions.orEmpty(),
                (theirs?.access as? PluginGrantAccess.B)?.contributions.orEmpty(),
            )
        if (contributions.size > 500) return mine
        return contributions
            .takeIf { it.isNotEmpty() }
            ?.let { PluginInstanceGrant(PluginGrantAccess.B(it), instanceId) }
    }

    return mine
}

private fun mergeCredentialSuppliers(
    base: List<String>,
    mine: List<String>,
    theirs: List<String>,
    packageByInstance: Map<String, String>,
): List<String> {
    fun packageKey(instance: String) = packageByInstance[instance] ?: "unknown:$instance"
    fun supplierFor(instances: List<String>, packageId: String) = instances.find {
        packageKey(it) == packageId
    }

    val packages = (base + mine + theirs).map(::packageKey).distinct().sorted()
    return packages.mapNotNull { packageId ->
        val baseSupplier = supplierFor(base, packageId)
        val mineSupplier = supplierFor(mine, packageId)
        val theirsSupplier = supplierFor(theirs, packageId)
        val supplier =
            when {
                mineSupplier == baseSupplier -> theirsSupplier
                theirsSupplier == baseSupplier || mineSupplier == theirsSupplier -> mineSupplier
                else -> mineSupplier
            }
        supplier
    }
}

private fun keepOnlySelectedCredentialContributions(
    grants: List<PluginInstanceGrant>,
    runCredentialInstances: List<String>,
    packageByInstance: Map<String, String>,
): List<PluginInstanceGrant> {
    fun packageKey(instance: String) = packageByInstance[instance] ?: "unknown:$instance"

    val supplierByPackage = runCredentialInstances.associateBy(::packageKey)
    return grants.mapNotNull { grant ->
        val access = grant.access as? PluginGrantAccess.B ?: return@mapNotNull grant
        if (access.contributions.none { it.kind == "runCredentials" }) return@mapNotNull grant
        if (supplierByPackage[packageKey(grant.instanceId)] == grant.instanceId) {
            return@mapNotNull grant
        }

        val contributions = access.contributions.filter { it.kind != "runCredentials" }
        if (contributions.isEmpty()) null
        else grant.copy(access = PluginGrantAccess.B(contributions))
    }
}

/** Rebase local grant edits on the latest owner-authored snapshot after a conflict. */
fun mergePluginGrantDrafts(
    base: PluginGrantDraft,
    mine: PluginGrantDraft,
    theirs: PluginGrantDraft,
    packageByInstance: Map<String, String> = emptyMap(),
): PluginGrantDraft {
    val baseByInstance = base.grants.associateBy { it.instanceId }
    val mineByInstance = mine.grants.associateBy { it.instanceId }
    val theirsByInstance = theirs.grants.associateBy { it.instanceId }
    val instances =
        (base.grants + mine.grants + theirs.grants).map { it.instanceId }.distinct().sorted()
    val grants = instances.mapNotNull { instanceId ->
        mergeGrant(
            baseByInstance[instanceId],
            mineByInstance[instanceId],
            theirsByInstance[instanceId],
            instanceId,
        )
    }

    val grantsByInstance = grants.associateBy { it.instanceId }
    val mergedSuppliers =
        mergeCredentialSuppliers(
            base.runCredentialInstances,
            mine.runCredentialInstances,
            theirs.runCredentialInstances,
            packageByInstance,
        )
    val runCredentialInstances = mergedSuppliers.filter { instanceId ->
        when (val access = grantsByInstance[instanceId]?.access) {
            is PluginGrantAccess.A -> true
            is PluginGrantAccess.B -> access.contributions.any { it.kind == "runCredentials" }
            null -> false
        }
    }
    val grantsWithSelectedCredentialContributions =
        keepOnlySelectedCredentialContributions(grants, runCredentialInstances, packageByInstance)

    return PluginGrantDraft(grantsWithSelectedCredentialContributions, runCredentialInstances)
}

data class GrantContribution(
    val grant: PluginContributionGrant,
    val description: String,
    val resourceTypes: List<PluginResourceType>,
)

fun staleResourceRestrictions(
    resources: List<PluginResourceRef>,
    selectableTypes: List<PluginResourceType>,
): List<PluginResourceRef> {
    val selectable = selectableTypes.mapTo(mutableSetOf()) { it.name }
    return resources.filterNot { it.resourceType in selectable }
}

private fun Char.isRustWhitespace(): Boolean =
    this in '\u0009'..'\u000D' ||
        this == '\u0020' ||
        this == '\u0085' ||
        this == '\u00A0' ||
        this == '\u1680' ||
        this in '\u2000'..'\u200A' ||
        this == '\u2028' ||
        this == '\u2029' ||
        this == '\u202F' ||
        this == '\u205F' ||
        this == '\u3000'

private fun String.hasNonRustWhitespace(): Boolean = any { !it.isRustWhitespace() }

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
    if (instructions.hasNonRustWhitespace())
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
                        if (after.grants.none { it.instanceId == id }) ReviewKind.Reduction
                        else ReviewKind.Access,
                        "$name : ${after.description(id)}. Effectif immédiatement, y compris dans les conversations en cours.",
                        requiresReview = after.grants.none { it.instanceId == id },
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
                ReviewKind.Access,
                "Fournisseurs d’identifiants de run : ${names.joinToString().ifBlank { "Aucun" }}. Les identifiants rendent vaines les restrictions de ressource des outils.",
            )
        )
    }
}
