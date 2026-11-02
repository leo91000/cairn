package build.cairn.app.data

import build.cairn.contracts.PluginArgument
import build.cairn.contracts.PluginCommand
import build.cairn.contracts.PluginContributionGrant
import build.cairn.contracts.PluginGrantAccess
import build.cairn.contracts.PluginHook
import build.cairn.contracts.PluginInstanceGrant
import build.cairn.contracts.PluginManifest
import build.cairn.contracts.PluginResourceField
import build.cairn.contracts.PluginResourceRef
import build.cairn.contracts.PluginResourceType
import build.cairn.contracts.PluginTool
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PluginGrantsTest {
    private val support = "11111111-1111-4111-8111-111111111111"
    private val automation = "22222222-2222-4222-8222-222222222222"
    private val otherPackage = "33333333-3333-4333-8333-333333333333"

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

        val text = pluginGrantReview(before, after, catalog = null).joinToString(" ") { it.text }
        val kinds = pluginGrantReview(before, after, catalog = null).map { it.kind }

        assertTrue(text.contains("channel support et descendants"))
        assertTrue(text.contains("Fournisseurs d’identifiants de run"))
        assertTrue(text.contains("rendent vaines les restrictions de ressource"))
        assertEquals(listOf(ReviewKind.Access, ReviewKind.Access), kinds)
    }

    @Test
    fun revokingPluginGrantRequiresAccessReview() {
        val before =
            PluginGrantDraft(grants = listOf(PluginInstanceGrant(PluginGrantAccess.A(), support)))

        val lines = pluginGrantReview(before, PluginGrantDraft(), catalog = null)

        assertEquals(1, lines.size)
        assertEquals(ReviewKind.Reduction, lines.single().kind)
        assertTrue(lines.single().requiresReview)
        assertTrue(needsReview(lines, preview = null))
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
}
