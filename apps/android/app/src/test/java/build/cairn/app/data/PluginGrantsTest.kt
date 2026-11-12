package build.cairn.app.data

import build.cairn.contracts.PluginArgument
import build.cairn.contracts.PluginCatalog
import build.cairn.contracts.PluginCommand
import build.cairn.contracts.PluginConnection
import build.cairn.contracts.PluginContributionGrant
import build.cairn.contracts.PluginEntry
import build.cairn.contracts.PluginGrantAccess
import build.cairn.contracts.PluginHook
import build.cairn.contracts.PluginInstanceGrant
import build.cairn.contracts.PluginManifest
import build.cairn.contracts.PluginResourceField
import build.cairn.contracts.PluginResourceRef
import build.cairn.contracts.PluginResourceType
import build.cairn.contracts.PluginStatus
import build.cairn.contracts.PluginTool
import build.cairn.contracts.PluginUsage
import kotlinx.serialization.json.buildJsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PluginGrantsTest {
    private val support = "11111111-1111-4111-8111-111111111111"
    private val automation = "22222222-2222-4222-8222-222222222222"
    private val otherPackage = "33333333-3333-4333-8333-333333333333"

    @Test
    fun rebasingPreservesEachRestrictedTypeAndIndependentRootRemovals() {
        val channel = PluginResourceRef("support", "channel")
        val general = PluginResourceRef("general", "channel")
        val alice = PluginResourceRef("alice", "author")
        val bob = PluginResourceRef("bob", "author")
        for (kind in listOf("tool", "command", "hook")) {
            fun draft(roots: List<PluginResourceRef>) =
                PluginGrantDraft(
                    listOf(
                        PluginInstanceGrant(
                            PluginGrantAccess.B(
                                listOf(PluginContributionGrant(kind, "read", roots))
                            ),
                            support,
                        )
                    )
                )
            val merged =
                mergePluginGrantDrafts(
                    draft(listOf(channel, general, alice)),
                    draft(listOf(channel, alice)),
                    draft(listOf(general, alice)),
                )
            val roots =
                (merged.grants.single().access as PluginGrantAccess.B)
                    .contributions
                    .single()
                    .resources
            assertEquals(listOf(alice, channel), roots)

            val independent =
                mergePluginGrantDrafts(
                    draft(listOf(channel, general, alice, bob)),
                    draft(listOf(channel, alice, bob)),
                    draft(listOf(general, alice)),
                )
            assertEquals(
                listOf(alice, channel),
                (independent.grants.single().access as PluginGrantAccess.B)
                    .contributions
                    .single()
                    .resources,
            )
        }
    }

    @Test
    fun nestedResourceAncestryIsShownFromItsRootToTheSelectedResource() {
        assertEquals(
            "Team server › Channels › Support thread",
            pluginResourcePathLabel(listOf("Channels", "Team server"), "Support thread"),
        )
    }

    @Test
    fun whitespaceOnlyInstructionsAreNotGrantable() {
        val manifest =
            PluginManifest(
                apiVersion = 2,
                capabilities = emptyList(),
                id = "example",
                instructions = " \n\t",
                name = "Example",
                version = "1.0.0",
            )

        assertFalse(manifest.grantContributions().any { it.grant.kind == "instructions" })
        assertFalse(
            manifest.copy(instructions = "\u0085").grantContributions().any {
                it.grant.kind == "instructions"
            }
        )
        assertTrue(
            manifest.copy(instructions = "\u001c").grantContributions().any {
                it.grant.kind == "instructions"
            }
        )
        assertTrue(
            manifest.copy(instructions = "Guide").grantContributions().any {
                it.grant.kind == "instructions"
            }
        )
    }

    @Test
    fun movingSupplierRemovesOnlyThePreviousSelectedCredentialContribution() {
        val tool = PluginContributionGrant("tool", "read", emptyList())
        val credentials = PluginContributionGrant("runCredentials", "", emptyList())
        val before =
            PluginGrantDraft(
                grants =
                    listOf(
                        PluginInstanceGrant(
                            PluginGrantAccess.B(listOf(tool, credentials)),
                            support,
                        ),
                        PluginInstanceGrant(PluginGrantAccess.A(), automation),
                        PluginInstanceGrant(
                            PluginGrantAccess.B(listOf(credentials)),
                            otherPackage,
                        ),
                    ),
                runCredentialInstances = listOf(support, otherPackage),
            )

        val after = before.chooseSupplier(listOf(support, automation), automation)

        assertEquals(
            listOf(tool),
            (after.grants.single { it.instanceId == support }.access as PluginGrantAccess.B)
                .contributions,
        )
        assertTrue(
            after.grants.single { it.instanceId == automation }.access is PluginGrantAccess.A
        )
        assertEquals(
            listOf(credentials),
            (after.grants.single { it.instanceId == otherPackage }.access as PluginGrantAccess.B)
                .contributions,
        )
        assertEquals(setOf(automation, otherPackage), after.runCredentialInstances.toSet())
    }

    @Test
    fun changingSelectedSupplierToFullAccessPreservesExplicitCredentialChoice() {
        val tool = PluginContributionGrant("tool", "read", emptyList())
        val credentials = PluginContributionGrant("runCredentials", "", emptyList())
        val before =
            PluginGrantDraft(
                grants =
                    listOf(
                        PluginInstanceGrant(
                            PluginGrantAccess.B(listOf(tool, credentials)),
                            support,
                        ),
                        PluginInstanceGrant(PluginGrantAccess.A(), automation),
                    ),
                runCredentialInstances = listOf(support),
            )

        val after = before.withAccess(support, PluginGrantAccess.A())

        val fullGrant = after.grants.single { it.instanceId == support }
        assertTrue(fullGrant.access is PluginGrantAccess.A)
        assertEquals(listOf(support), after.runCredentialInstances)
        assertEquals(emptyList<String>(), before.withAccess(support, null).runCredentialInstances)
        assertEquals(
            emptyList<String>(),
            before.withAccess(support, PluginGrantAccess.B(listOf(tool))).runCredentialInstances,
        )
    }

    @Test
    fun removingOneStaleContributionPreservesOtherPermissionsAndSupplier() {
        val tool = PluginContributionGrant("tool", "read", emptyList())
        val credentials = PluginContributionGrant("runCredentials", "", emptyList())
        val command = PluginContributionGrant("command", "post", emptyList())
        val before =
            PluginGrantDraft(
                grants =
                    listOf(
                        PluginInstanceGrant(
                            PluginGrantAccess.B(listOf(tool, command, credentials)),
                            support,
                        ),
                        PluginInstanceGrant(PluginGrantAccess.A(), automation),
                    ),
                runCredentialInstances = listOf(support),
            )

        val after = before.withoutContribution(support, "tool", "read")

        assertEquals(
            listOf(command, credentials),
            (after.grants.single { it.instanceId == support }.access as PluginGrantAccess.B)
                .contributions,
        )
        assertEquals(listOf(support), after.runCredentialInstances)
        val withoutCredentials = after.withoutContribution(support, "runCredentials", "")
        assertEquals(
            listOf(command),
            (withoutCredentials.grants.single { it.instanceId == support }.access
                    as PluginGrantAccess.B)
                .contributions,
        )
        assertTrue(withoutCredentials.runCredentialInstances.isEmpty())
    }

    @Test
    fun removingSupplierDoesNotChooseAnotherFullAccessInstance() {
        val before =
            PluginGrantDraft(
                grants =
                    listOf(
                        PluginInstanceGrant(PluginGrantAccess.A(), support),
                        PluginInstanceGrant(PluginGrantAccess.A(), automation),
                    ),
                runCredentialInstances = listOf(support),
            )

        val after = before.chooseSupplier(listOf(support, automation), null)

        assertEquals(emptyList<String>(), after.runCredentialInstances)
        assertTrue(after.grants.all { it.access is PluginGrantAccess.A })
    }

    @Test
    fun rebasingConcurrentGrantChangesKeepsIndependentContributionsAndResources() {
        val readAtSupport =
            PluginContributionGrant(
                "tool",
                "read",
                listOf(PluginResourceRef("support", "channel")),
            )
        val base =
            PluginGrantDraft(
                listOf(PluginInstanceGrant(PluginGrantAccess.B(listOf(readAtSupport)), support))
            )
        val mine =
            PluginGrantDraft(
                listOf(
                    PluginInstanceGrant(
                        PluginGrantAccess.B(
                            listOf(
                                readAtSupport.copy(
                                    resources =
                                        readAtSupport.resources +
                                            PluginResourceRef("general", "channel")
                                )
                            )
                        ),
                        support,
                    ),
                    PluginInstanceGrant(PluginGrantAccess.A(), automation),
                ),
                runCredentialInstances = listOf(automation),
            )
        val theirs =
            PluginGrantDraft(
                listOf(
                    PluginInstanceGrant(
                        PluginGrantAccess.B(
                            listOf(
                                readAtSupport.copy(
                                    resources =
                                        readAtSupport.resources +
                                            PluginResourceRef("team", "server")
                                )
                            )
                        ),
                        support,
                    ),
                    PluginInstanceGrant(
                        PluginGrantAccess.B(
                            listOf(
                                PluginContributionGrant("hook", "message", emptyList()),
                                PluginContributionGrant("runCredentials", "", emptyList()),
                            )
                        ),
                        otherPackage,
                    ),
                ),
                runCredentialInstances = listOf(otherPackage),
            )

        val merged =
            mergePluginGrantDrafts(
                base,
                mine,
                theirs,
                mapOf(automation to "automation-package", otherPackage to "other-package"),
            )

        val mergedRead =
            (merged.grants.single { it.instanceId == support }.access as PluginGrantAccess.B)
                .contributions
                .single()
        assertEquals(
            listOf(
                PluginResourceRef("general", "channel"),
                PluginResourceRef("support", "channel"),
                PluginResourceRef("team", "server"),
            ),
            mergedRead.resources,
        )
        assertEquals(
            setOf(support, automation, otherPackage),
            merged.grants.map { it.instanceId }.toSet(),
        )
        assertEquals(setOf(automation, otherPackage), merged.runCredentialInstances.toSet())
    }

    @Test
    fun rebasingPreservesLocalNoAccessWhenTheSameGrantChangedRemotely() {
        val baseGrant =
            PluginInstanceGrant(
                PluginGrantAccess.B(
                    listOf(
                        PluginContributionGrant(
                            "tool",
                            "read",
                            listOf(PluginResourceRef("support", "channel")),
                        )
                    )
                ),
                support,
            )
        val base = PluginGrantDraft(listOf(baseGrant))
        val mine = PluginGrantDraft()
        val theirs =
            PluginGrantDraft(
                listOf(
                    baseGrant.copy(
                        access =
                            PluginGrantAccess.B(
                                listOf(
                                    PluginContributionGrant(
                                        "tool",
                                        "read",
                                        listOf(
                                            PluginResourceRef("support", "channel"),
                                            PluginResourceRef("general", "channel"),
                                        ),
                                    )
                                )
                            )
                    )
                )
            )

        assertEquals(mine, mergePluginGrantDrafts(base, mine, theirs))
    }

    @Test
    fun rebasingRootRemovalsCannotCreateAnUnrestrictedResourceGrant() {
        val base =
            PluginGrantDraft(
                listOf(
                    PluginInstanceGrant(
                        PluginGrantAccess.B(
                            listOf(
                                PluginContributionGrant(
                                    "tool",
                                    "read",
                                    listOf(
                                        PluginResourceRef("support", "channel"),
                                        PluginResourceRef("general", "channel"),
                                    ),
                                )
                            )
                        ),
                        support,
                    )
                )
            )
        val mine =
            PluginGrantDraft(
                listOf(
                    PluginInstanceGrant(
                        PluginGrantAccess.B(
                            listOf(
                                PluginContributionGrant(
                                    "tool",
                                    "read",
                                    listOf(PluginResourceRef("general", "channel")),
                                )
                            )
                        ),
                        support,
                    )
                )
            )
        val theirs =
            PluginGrantDraft(
                listOf(
                    PluginInstanceGrant(
                        PluginGrantAccess.B(
                            listOf(
                                PluginContributionGrant(
                                    "tool",
                                    "read",
                                    listOf(PluginResourceRef("support", "channel")),
                                )
                            )
                        ),
                        support,
                    )
                )
            )

        assertEquals(mine, mergePluginGrantDrafts(base, mine, theirs))
    }

    @Test
    fun rebasingKeepsSuppliersForUnknownInstancesDistinct() {
        val grants =
            listOf(
                PluginInstanceGrant(PluginGrantAccess.A(), support),
                PluginInstanceGrant(PluginGrantAccess.A(), automation),
            )
        val merged =
            mergePluginGrantDrafts(
                PluginGrantDraft(grants),
                PluginGrantDraft(grants, listOf(support)),
                PluginGrantDraft(grants, listOf(automation)),
            )

        assertEquals(setOf(automation, support), merged.runCredentialInstances.toSet())
    }

    @Test
    fun rebasingKeepsOneSupplierWhenConcurrentChangesChooseDifferentInstancesOfTheSamePackage() {
        val review = "44444444-4444-4444-8444-444444444444"
        val base =
            PluginGrantDraft(
                grants = listOf(PluginInstanceGrant(PluginGrantAccess.A(), support)),
                runCredentialInstances = listOf(support),
            )
        val mine =
            PluginGrantDraft(
                grants =
                    listOf(
                        PluginInstanceGrant(PluginGrantAccess.A(), support),
                        PluginInstanceGrant(PluginGrantAccess.A(), automation),
                    ),
                runCredentialInstances = listOf(automation),
            )
        val theirs =
            PluginGrantDraft(
                grants =
                    listOf(
                        PluginInstanceGrant(PluginGrantAccess.A(), support),
                        PluginInstanceGrant(PluginGrantAccess.A(), review),
                    ),
                runCredentialInstances = listOf(review),
            )

        val merged =
            mergePluginGrantDrafts(
                base,
                mine,
                theirs,
                mapOf(
                    support to "grant-ui",
                    automation to "grant-ui",
                    review to "grant-ui",
                ),
            )

        assertEquals(listOf(automation), merged.runCredentialInstances)
    }

    @Test
    fun rebasingRemovesCredentialsFromSamePackageInstancesThatLoseSupplierChoice() {
        val credentials = PluginContributionGrant("runCredentials", "", emptyList())
        val messageHook = PluginContributionGrant("hook", "message", emptyList())
        val readTool = PluginContributionGrant("tool", "read", emptyList())
        val base = PluginGrantDraft()
        val mine =
            PluginGrantDraft(
                grants =
                    listOf(
                        PluginInstanceGrant(
                            PluginGrantAccess.B(listOf(credentials, messageHook)),
                            support,
                        )
                    ),
                runCredentialInstances = listOf(support),
            )
        val theirs =
            PluginGrantDraft(
                grants =
                    listOf(
                        PluginInstanceGrant(
                            PluginGrantAccess.B(listOf(credentials, readTool)),
                            automation,
                        )
                    ),
                runCredentialInstances = listOf(automation),
            )

        val merged =
            mergePluginGrantDrafts(
                base,
                mine,
                theirs,
                mapOf(support to "grant-ui", automation to "grant-ui"),
            )

        assertEquals(listOf(support), merged.runCredentialInstances)
        assertEquals(
            setOf(credentials, messageHook),
            (merged.grants.single { it.instanceId == support }.access as PluginGrantAccess.B)
                .contributions
                .toSet(),
        )
        assertEquals(
            listOf(readTool),
            (merged.grants.single { it.instanceId == automation }.access as PluginGrantAccess.B)
                .contributions,
        )
    }

    @Test
    fun rebasingDropsSupplierWhenConcurrentEditRevokesItsGrant() {
        val base =
            PluginGrantDraft(
                grants = listOf(PluginInstanceGrant(PluginGrantAccess.A(), automation))
            )
        val mine = base.copy(runCredentialInstances = listOf(automation))
        val theirs = PluginGrantDraft()

        val merged =
            mergePluginGrantDrafts(
                base,
                mine,
                theirs,
                mapOf(automation to "automation-package"),
            )

        assertEquals(theirs, merged)
    }

    @Test
    fun rebasingDropsSupplierWhenConcurrentEditRemovesCredentialPermission() {
        val tool = PluginContributionGrant("tool", "read", emptyList())
        val base =
            PluginGrantDraft(
                grants = listOf(PluginInstanceGrant(PluginGrantAccess.A(), automation))
            )
        val mine = base.copy(runCredentialInstances = listOf(automation))
        val theirs =
            PluginGrantDraft(
                grants = listOf(PluginInstanceGrant(PluginGrantAccess.B(listOf(tool)), automation))
            )

        val merged =
            mergePluginGrantDrafts(
                base,
                mine,
                theirs,
                mapOf(automation to "automation-package"),
            )

        assertEquals(theirs, merged)
    }

    @Test
    fun grantReviewNamesDescendantCoverageAndCredentialSupplierEffects() {
        val before = PluginGrantDraft()
        val after =
            PluginGrantDraft(
                grants =
                    listOf(
                        PluginInstanceGrant(
                            PluginGrantAccess.B(
                                listOf(
                                    PluginContributionGrant(
                                        "tool",
                                        "read",
                                        listOf(PluginResourceRef("support", "channel")),
                                    )
                                )
                            ),
                            support,
                        )
                    ),
                runCredentialInstances = listOf(support),
            )

        val lines = pluginGrantReview(before, after, catalog = null)
        val (grant, credentials) = lines

        assertEquals(listOf(ReviewKind.Access, ReviewKind.Access), lines.map { it.kind })
        assertEquals(support, grant.title)
        assertEquals("Aucun accès → 1 contribution", grant.change)
        assertEquals("$support : Aucun accès → 1 contribution", grant.text)
        // Without a catalog or a resolved path, the root falls back to its type and id.
        assertEquals("Read (Outil) : Channel · support et ses descendants", grant.details.first())
        assertTrue(grant.details.last().contains("identifiants déjà chargés restent utilisables"))
        assertEquals("Identifiants de run", credentials.title)
        assertEquals("Aucun → $support", credentials.change)
        assertTrue(
            credentials.details.single().contains("rendent vaines les restrictions de ressource")
        )
    }

    @Test
    fun grantReviewDistinguishesSameNamedInstancesFromDifferentPackages() {
        val catalog =
            PluginCatalog(
                capabilities = emptyList(),
                connections =
                    listOf(
                        PluginConnection(
                            id = support,
                            name = "Main",
                            pluginId = "tickets",
                            settings = buildJsonObject {},
                            status = PluginStatus(),
                            usage = PluginUsage(),
                        ),
                        PluginConnection(
                            id = automation,
                            name = "Main",
                            pluginId = "deployments",
                            settings = buildJsonObject {},
                            status = PluginStatus(),
                            usage = PluginUsage(),
                        ),
                    ),
                packages =
                    listOf(
                        PluginEntry(
                            active = null,
                            builtin = false,
                            id = "tickets",
                            manifest =
                                PluginManifest(
                                    apiVersion = 2,
                                    capabilities = emptyList(),
                                    id = "tickets",
                                    name = "Tickets",
                                    version = "1",
                                ),
                            setup = null,
                            status = "active",
                            versions = emptyList(),
                        ),
                        PluginEntry(
                            active = null,
                            builtin = false,
                            id = "deployments",
                            manifest =
                                PluginManifest(
                                    apiVersion = 2,
                                    capabilities = emptyList(),
                                    id = "deployments",
                                    name = "Deployments",
                                    version = "1",
                                ),
                            setup = null,
                            status = "active",
                            versions = emptyList(),
                        ),
                    ),
            )
        val after =
            PluginGrantDraft(
                grants =
                    listOf(
                        PluginInstanceGrant(PluginGrantAccess.A(), support),
                        PluginInstanceGrant(PluginGrantAccess.A(), automation),
                    ),
                runCredentialInstances = listOf(support, automation),
            )

        val lines = pluginGrantReview(PluginGrantDraft(), after, catalog)

        assertEquals(3, lines.size)
        assertEquals("Tickets · Main", lines[0].title)
        assertEquals("Aucun accès → Accès complet", lines[0].change)
        assertEquals("Deployments · Main", lines[1].title)
        assertEquals("Aucun → Tickets · Main, Deployments · Main", lines[2].change)
    }

    @Test
    fun revokingPluginGrantRequiresAccessReview() {
        val before =
            PluginGrantDraft(grants = listOf(PluginInstanceGrant(PluginGrantAccess.A(), support)))

        val lines = pluginGrantReview(before, PluginGrantDraft(), catalog = null)

        assertEquals(1, lines.size)
        assertEquals(ReviewKind.Reduction, lines.single().kind)
        assertTrue(lines.single().requiresReview)
        assertEquals("Accès complet → Aucun accès", lines.single().change)
        assertTrue(
            lines
                .single()
                .details
                .single()
                .contains("identifiants déjà chargés restent utilisables")
        )
        assertTrue(needsReview(lines, preview = null))
    }

    @Test
    fun removingRunCredentialSupplierExplainsActiveRunRetention() {
        val before =
            PluginGrantDraft(
                grants = listOf(PluginInstanceGrant(PluginGrantAccess.A(), support)),
                runCredentialInstances = listOf(support),
            )

        val after = before.copy(runCredentialInstances = emptyList())

        val line = pluginGrantReview(before, after, catalog = null).single()

        assertEquals("Identifiants de run", line.title)
        assertEquals("$support → Aucun", line.change)
        assertTrue(
            line.details
                .single()
                .contains("Les runs déjà en cours conservent les identifiants chargés")
        )
    }

    @Test
    fun onlyResourceTypesMatchingDeclaredContributionFieldsAreSelectable() {
        val manifest =
            PluginManifest(
                id = "grant-ui",
                name = "Grant UI",
                version = "1",
                apiVersion = 2,
                capabilities = emptyList(),
                resources =
                    listOf(
                        PluginResourceType("Servers", "server"),
                        PluginResourceType("Channels", "channel", "server"),
                        PluginResourceType("Threads", "thread", "channel"),
                        PluginResourceType("Authors", "author"),
                    ),
                tools =
                    listOf(
                        PluginTool(
                            "Read",
                            kotlinx.serialization.json.JsonNull,
                            "read",
                            listOf(PluginResourceField("/channel", "channel")),
                        ),
                        PluginTool(
                            "Unrestricted",
                            kotlinx.serialization.json.JsonNull,
                            "unrestricted",
                        ),
                    ),
                commands =
                    listOf(
                        PluginCommand(
                            name = "post",
                            arguments =
                                listOf(PluginArgument(name = "channel", resourceType = "channel")),
                        )
                    ),
                hooks =
                    listOf(
                        PluginHook(
                            "Message",
                            "message",
                            kotlinx.serialization.json.JsonNull,
                            listOf(PluginResourceField("/channel", "channel")),
                        )
                    ),
            )

        val contributions = manifest.grantContributions()
        val expected = listOf("server", "channel")

        assertEquals(
            expected,
            contributions.first { it.grant.name == "read" }.resourceTypes.map { it.name },
        )
        assertEquals(
            expected,
            contributions.first { it.grant.kind == "command" }.resourceTypes.map { it.name },
        )
        assertEquals(
            expected,
            contributions.first { it.grant.kind == "hook" }.resourceTypes.map { it.name },
        )
        assertTrue(contributions.first { it.grant.name == "unrestricted" }.resourceTypes.isEmpty())
    }

    @Test
    fun savedResourceRootsForRemovedContributionTypesAreStale() {
        val resources =
            listOf(
                PluginResourceRef("support", "channel"),
                PluginResourceRef("engineering", "team"),
            )
        val currentTypes = listOf(PluginResourceType("Channels", "channel"))

        assertEquals(
            listOf(PluginResourceRef("engineering", "team")),
            staleResourceRestrictions(resources, currentTypes),
        )
        assertEquals(resources, staleResourceRestrictions(resources, emptyList()))
    }

    @Test
    fun contributionAndResourceLabelsComeFromTheManifestWithAReadableFallback() {
        val manifest =
            PluginManifest(
                id = "grant-ui",
                name = "Grant UI",
                version = "1",
                apiVersion = 2,
                capabilities = emptyList(),
                resources =
                    listOf(
                        PluginResourceType("Servers", "server"),
                        PluginResourceType(" ", "channel", "server"),
                    ),
                tools =
                    listOf(
                        PluginTool("  Read messages ", kotlinx.serialization.json.JsonNull, "read"),
                        PluginTool("", kotlinx.serialization.json.JsonNull, "grant_ui-read"),
                    ),
                commands = listOf(PluginCommand(name = "post")),
                runCredentials = build.cairn.contracts.PluginRunCredentials(),
                instructions = "Guide",
            )

        val labels = manifest.grantContributions().map { it.label to it.kindLabel }

        assertEquals(
            listOf(
                "Read messages" to "Outil",
                "Grant ui read" to "Outil",
                "Post" to "Commande",
                "Identifiants de run" to "Identifiants",
                "Instructions" to "Instructions",
            ),
            labels,
        )
        assertEquals("Événement", pluginContributionKindLabel("hook"))
        assertEquals("Source de projets", pluginContributionKindLabel("projectSource"))
        assertEquals("Source de skills", pluginContributionKindLabel("skillSource"))
        assertEquals("Condition", pluginContributionKindLabel("condition"))

        val (server, channel) = manifest.resources
        assertEquals("Servers", pluginResourceTypeLabel(server))
        assertEquals("Channel", pluginResourceTypeLabel(channel))
        assertEquals(
            "Servers › Channel",
            pluginResourceTypeHierarchyLabel(channel, manifest.resources),
        )
        assertEquals(
            "Thread · t1",
            pluginResourceFallbackLabel(PluginResourceRef("t1", "thread"), manifest.resources),
        )
    }

    @Test
    fun grantReviewListsEachContributionWithResolvedRootsAndReadableKinds() {
        val catalog =
            PluginCatalog(
                capabilities = emptyList(),
                connections =
                    listOf(
                        PluginConnection(
                            id = support,
                            name = "Support",
                            pluginId = "grant-ui",
                            settings = buildJsonObject {},
                            status = PluginStatus(),
                            usage = PluginUsage(),
                        )
                    ),
                packages =
                    listOf(
                        PluginEntry(
                            active = null,
                            builtin = false,
                            id = "grant-ui",
                            manifest =
                                PluginManifest(
                                    apiVersion = 2,
                                    capabilities = emptyList(),
                                    id = "grant-ui",
                                    name = "Grant UI",
                                    version = "1",
                                    resources = listOf(PluginResourceType("Channels", "channel")),
                                    tools =
                                        listOf(
                                            PluginTool(
                                                "Read messages",
                                                kotlinx.serialization.json.JsonNull,
                                                "read",
                                                listOf(PluginResourceField("/channel", "channel")),
                                            )
                                        ),
                                    hooks =
                                        listOf(
                                            PluginHook(
                                                "Receive messages",
                                                "message",
                                                kotlinx.serialization.json.JsonNull,
                                            )
                                        ),
                                ),
                            setup = null,
                            status = "active",
                            versions = emptyList(),
                        )
                    ),
            )
        val support1 = PluginResourceRef("support", "channel")
        val general = PluginResourceRef("general", "channel")
        val after =
            PluginGrantDraft(
                grants =
                    listOf(
                        PluginInstanceGrant(
                            PluginGrantAccess.B(
                                listOf(
                                    PluginContributionGrant(
                                        "tool",
                                        "read",
                                        listOf(support1, general),
                                    ),
                                    PluginContributionGrant("hook", "message", emptyList()),
                                    PluginContributionGrant("command", "gone_away", emptyList()),
                                )
                            ),
                            support,
                        )
                    )
            )

        val line =
            pluginGrantReview(
                    PluginGrantDraft(
                        grants = listOf(PluginInstanceGrant(PluginGrantAccess.A(), support))
                    ),
                    after,
                    catalog,
                    mapOf(pluginResourcePathKey(support, support1) to "Team › #support"),
                )
                .single()

        assertEquals(ReviewKind.Access, line.kind)
        assertFalse(line.requiresReview)
        assertEquals("Grant UI · Support", line.title)
        assertEquals("Accès complet → 3 contributions", line.change)
        assertEquals("Grant UI · Support : Accès complet → 3 contributions", line.text)
        assertEquals(
            listOf(
                "Read messages (Outil) : Team › #support, Channels · general et leurs descendants",
                "Receive messages (Événement) : toutes les ressources",
                "Gone away (Commande) : toutes les ressources",
            ),
            line.details.dropLast(1),
        )
        assertTrue(line.details.last().startsWith("Les contrôles d’accès du plugin"))
        assertTrue(needsReview(listOf(line), preview = null))
    }
}
