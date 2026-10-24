package build.cairn.app.data

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AgentEditingTest {
    private val gpu =
        ExecutionNode(
            "gpu",
            "gpu-box",
            accepting = true,
            status = "online",
            exclusiveReservations = true,
            executionReady = true,
        )
    private val mac = ExecutionNode("mac", "mac-mini", status = "offline")
    private val nodes = listOf(gpu, mac)
    private val projects = listOf(Project("p1", "cairn"), Project("p2", "SheetOM"))
    private val catalog =
        PluginCatalog(
            packages =
                listOf(
                    PluginEntry(
                        "onepassword",
                        manifest =
                            PluginManifest(
                                id = "onepassword",
                                name = "1Password",
                                instances = PluginInstances("compte", "comptes"),
                                agentAccess = PluginAgentAccess("Lecture seule"),
                            ),
                    ),
                    PluginEntry(
                        "discord",
                        manifest = PluginManifest(id = "discord", name = "Discord"),
                    ),
                ),
            connections =
                listOf(
                    PluginConnection("prod", "onepassword", "Production"),
                    PluginConnection("staging", "onepassword", "Staging", enabled = false),
                    PluginConnection("bot", "discord", "#support", agentId = "agent"),
                ),
        )
    private val agent =
        Agent(
            id = "agent",
            name = "Release engineer",
            revision = "r1",
            access =
                AccessPolicy(nodes = listOf(LOCAL_NODE_ID, "gpu"), projects = listOf("p1", "p2")),
        )
    private val resources =
        AgentResources(projects, nodes = nodes, catalog = catalog.agentDirectory())

    @Test
    fun newFieldsDecodeWithDefaultsAndUnknownKeys() {
        val old =
            wireJson.decodeFromString<Agent>(
                """{"id":"a","name":"Old","access":{"projects":null,"future":1},"later":true}"""
            )
        assertEquals(emptyList<String>(), old.exclusiveNodes)
        assertEquals(emptyList<String>(), old.plugins)
        assertEquals("", old.revision)

        // Live grants sit next to the access policy, never inside it.
        val current =
            wireJson.decodeFromString<Agent>(
                """{"id":"a","name":"New","revision":"r9","access":{},"exclusiveNodes":["gpu"],"plugins":["prod"]}"""
            )
        assertEquals(listOf("gpu"), current.exclusiveNodes)
        assertEquals(listOf("prod"), current.plugins)
        assertEquals("r9", current.revision)
    }

    @Test
    fun saveBodySendsExplicitAllSelectionsRevisionAndReapprovals() {
        val body =
            agentSaveBody(
                agent.copy(
                    access = agent.access.copy(nodes = null, skills = null),
                    plugins = listOf("prod"),
                ),
                listOf("bot"),
            )
        val access = body["access"]!!.jsonObject
        assertEquals(JsonNull, access["nodes"])
        assertEquals(JsonNull, access["skills"])
        assertEquals(JsonNull, access["mcps"])
        assertEquals(2, access["projects"]!!.jsonArray.size)
        assertEquals("prod", body["plugins"]!!.jsonArray.single().jsonPrimitive.content)
        assertFalse("plugins" in access)
        assertFalse("exclusiveNodes" in access)
        assertEquals("r1", body["revision"]!!.jsonPrimitive.content)
        assertEquals("bot", body["reapproveDelegations"]!!.jsonArray.single().jsonPrimitive.content)
        assertFalse("avatar" in body)

        // A legacy record without a revision, or a new agent, sends none.
        val created = agentSaveBody(Agent(name = "New"))
        assertFalse("revision" in created)
        assertFalse("reapproveDelegations" in created)
    }

    @Test
    fun exclusiveNeedsExecutionAndStoppingExecutionDropsIt() {
        val granted = agent.withExclusive("gpu", true)
        assertEquals(listOf(gpu), granted.effectiveExclusive(nodes))
        assertEquals("2 sur 3 · exclusivité sur 1", nodesSummary(granted, nodes))

        val stopped = granted.withNodeRun("gpu", false)
        assertEquals(listOf(LOCAL_NODE_ID), stopped.access.nodes)
        assertTrue(stopped.exclusiveNodes.isEmpty())
        assertEquals("Runner actuel seulement", nodesSummary(stopped, nodes))

        // Legacy data: an exclusive grant where the agent cannot run is inactive and reported.
        val legacy = stopped.copy(exclusiveNodes = listOf("gpu"))
        assertTrue(legacy.effectiveExclusive(nodes).isEmpty())
        assertEquals(AgentSection.Nodes, agentIssues(legacy, resources).single().section)

        // "All nodes" counts every node as executable; turning it off keeps today's machines.
        val all = legacy.copy(access = legacy.access.withAllNodes(true, nodes))
        assertEquals(listOf(gpu), all.effectiveExclusive(nodes))
        assertEquals(
            listOf(LOCAL_NODE_ID, "gpu", "mac"),
            all.access.withAllNodes(false, nodes).nodes,
        )
    }

    @Test
    fun summariesUseTheSharedWording() {
        assertEquals("Tous, y compris futurs", selectionSummary(null, 12))
        assertEquals("Toutes, y compris futures", selectionSummary(null, 12, feminine = true))
        assertEquals("3 sur 12", selectionSummary(listOf("a", "b", "c"), 12))
        assertEquals("Aucun", selectionSummary(emptyList(), 12))
        assertEquals("", pluginSummary(agent, catalog.agentDirectory()))
        assertEquals(
            "1Password : 2 comptes",
            pluginSummary(
                agent.copy(plugins = listOf("prod", "staging")),
                catalog.agentDirectory(),
            ),
        )
        assertEquals("review · Global", skillLabel(Skill("review", scope = "global"), projects))
        assertEquals("lint · cairn", skillKeyLabel("p1/lint", projects))
        assertEquals("2 projets · 2 nodes", agentAccessSummary(agent))
        assertEquals("Limite : 120 min", timeoutSummary(120))
    }

    @Test
    fun changesAreCountedPerSetting() {
        val draft = AgentDraft(agent)
        assertTrue(agentChanges(agent, draft).isEmpty())

        val edited =
            draft.copy(
                agent =
                    agent.copy(
                        name = "Releases",
                        plugins = listOf("prod"),
                        exclusiveNodes = listOf("gpu"),
                    )
            )
        val changes = agentChanges(agent, edited)
        assertEquals(
            listOf(AgentSection.Identity, AgentSection.Plugins, AgentSection.Nodes),
            changes,
        )
        assertEquals("3 modifications non enregistrées", changesLabel(changes.size))
        assertTrue(agentErrors(edited).isEmpty())
    }

    @Test
    fun invalidDraftsPointToTheirSection() {
        val errors = agentErrors(AgentDraft(agent.copy(name = " "), timeout = "900"))
        assertEquals(setOf(AgentSection.Identity, AgentSection.Advanced), errors.keys)
        assertEquals(AgentSection.Nodes, sectionOfError("Exclusive agent missing."))
        assertEquals(
            AgentSection.Plugins,
            sectionOfError("A selected plugin instance no longer exists."),
        )
        // Two accounts of a package whose runs use their credentials: the server's refusal is
        // shown in the plugin grants.
        assertEquals(
            AgentSection.Plugins,
            sectionOfError(
                "Grant one GitHub instance per agent: its runs use that instance's credentials."
            ),
        )
        assertNull(sectionOfError("Something else"))
    }

    @Test
    fun reviewListsPowersSecretsAndWorkAReductionStops() {
        val edited =
            agent.copy(
                access = agent.access.copy(projects = listOf("p1"), installation = true),
                exclusiveNodes = listOf("gpu"),
                plugins = listOf("prod"),
            )
        val preview =
            AgentPreview(
                AgentPreviewRuns(queued = 2, running = 1),
                listOf(AgentDelegationEffect("bot", "discord", "#support", suspended = true)),
            )
        val lines = agentReview(agent, edited, resources, preview)
        assertEquals(
            listOf(ReviewKind.Power, ReviewKind.Power, ReviewKind.Access, ReviewKind.Reduction),
            lines.map { it.kind },
        )
        assertTrue(lines[0].text.startsWith("Ajout : réservation exclusive sur gpu-box."))
        assertEquals(
            "Ajout : 1Password « Production », effectif immédiatement, y compris dans les conversations en cours.",
            lines[2].text,
        )
        assertEquals(
            "Retrait : projet « SheetOM ». 2 exécutions en file et 1 conversation en cours ne pourront pas continuer avec les anciens accès.",
            lines[3].text,
        )
        assertTrue(needsReview(lines, preview))
        assertEquals(listOf("bot"), preview.suspendedDelegations().map { it.id })

        // A harmless change saves directly.
        val renamed = agent.copy(name = "Renamed")
        val quiet = agentReview(agent, renamed, resources, AgentPreview())
        assertTrue(quiet.isEmpty())
        assertFalse(needsReview(quiet, AgentPreview()))

        // A reduction that stops no work is listed but does not require the review.
        val reduced =
            agentReview(
                agent,
                agent.copy(access = agent.access.copy(projects = listOf("p1"))),
                resources,
                AgentPreview(),
            )
        assertEquals(listOf(ReviewKind.Reduction), reduced.map { it.kind })
        assertFalse(needsReview(reduced, AgentPreview()))
    }

    @Test
    fun reviewListsEveryRemovedGrantAndPower() {
        val granted =
            agent.copy(
                access = agent.access.copy(installation = true),
                exclusiveNodes = listOf("gpu"),
                plugins = listOf("prod"),
            )
        assertEquals(
            listOf(
                "réservation exclusive sur « gpu-box »",
                "1Password « Production »",
                "gestion de l’installation",
            ),
            agentReductions(granted, agent, resources),
        )
        val lines = agentReview(granted, agent, resources, AgentPreview())
        assertEquals(
            "Retrait : réservation exclusive sur « gpu-box », 1Password « Production », gestion de l’installation.",
            lines.single().text,
        )
        assertEquals(ReviewKind.Reduction, lines.single().kind)

        // "All" turned into a choice of nothing says so.
        val everything = agent.copy(access = agent.access.copy(skills = null, mcps = null))
        assertEquals(
            listOf("plus aucune skill", "plus aucune connexion MCP"),
            agentReductions(
                everything,
                everything.copy(
                    access = everything.access.copy(skills = emptyList(), mcps = emptyList())
                ),
                resources,
            ),
        )
    }

    @Test
    fun onlyNarrowerMcpToolChoicesAreReviewedAsRestrictions() {
        val chosen =
            agent.copy(
                access = agent.access.copy(mcps = null, mcpTools = mapOf("m" to listOf("a", "b")))
            )
        fun tools(vararg names: String) =
            chosen.copy(access = chosen.access.copy(mcpTools = mapOf("m" to names.toList())))
        assertEquals(emptyList<String>(), agentReductions(chosen, tools("a", "b", "c"), resources))
        assertEquals(
            listOf("outils MCP restreints"),
            agentReductions(chosen, tools("a"), resources),
        )
        val all = chosen.copy(access = chosen.access.copy(mcpTools = emptyMap()))
        assertEquals(listOf("outils MCP restreints"), agentReductions(all, tools("a"), resources))
        assertEquals(emptyList<String>(), agentReductions(chosen, all, resources))
    }

    @Test
    fun onlyDelegationsThisSaveSuspendsArePreselectedForReapproval() {
        val preview =
            AgentPreview(
                delegations =
                    listOf(
                        AgentDelegationEffect("new", suspended = true),
                        AgentDelegationEffect("earlier", suspended = true, alreadySuspended = true),
                        AgentDelegationEffect(
                            "blocked",
                            suspended = true,
                            blocked = "Accès refusé",
                        ),
                        AgentDelegationEffect("kept"),
                    )
            )
        assertEquals(setOf("new"), preview.preselectedReapprovals())
        assertEquals(
            listOf("new", "earlier", "blocked"),
            preview.suspendedDelegations().map { it.id },
        )
        // Older servers omit the flag: it means not already suspended.
        val decoded =
            wireJson.decodeFromString<AgentDelegationEffect>("""{"id":"bot","suspended":true}""")
        assertFalse(decoded.alreadySuspended)
    }

    @Test
    fun restrictingProjectsStartsFromNoneAndIsReviewedAsAReduction() {
        val all = agent.copy(access = agent.access.copy(projects = null))
        // Choosing starts with no project selected, as on the web.
        val draft = AgentDraft(all).withAllProjects(false)
        assertEquals(emptyList<String>(), draft.agent.access.projects)
        assertEquals(listOf(AgentSection.Projects), agentChanges(all, draft))
        val lines = agentReview(all, draft.resolved(), resources, null)
        assertEquals(listOf("Retrait : plus aucun projet."), lines.map { it.text })
        assertFalse(needsReview(lines, null))
        assertEquals(null, draft.withAllProjects(true).agent.access.projects)
    }

    @Test
    fun aReloadMergesSelectionsEntryByEntry() {
        // Untouched: the newer value.
        assertEquals(listOf("b"), mergeList(listOf("a"), listOf("a"), listOf("b")))
        // Both edited: theirs without what I removed, plus what I added.
        assertEquals(
            listOf("c", "d", "e"),
            mergeList(listOf("a", "b"), listOf("b", "e"), listOf("a", "b", "c", "d").minus("b")),
        )
        assertEquals(
            listOf("b", "c", "x"),
            mergeList(listOf("a", "b"), listOf("b", "x"), listOf("a", "b", "c")),
        )
        // All ↔ a choice: whichever side switched, mine first.
        assertEquals(listOf("a"), mergeList(null, listOf("a"), listOf("z")))
        assertNull(mergeList(listOf("a"), null, listOf("a", "b")))
        assertNull(mergeList(listOf("a"), listOf("a", "x"), null))

        val base =
            agent.copy(
                access =
                    agent.access.copy(mcps = listOf("m"), mcpTools = mapOf("m" to listOf("t1"))),
                plugins = listOf("prod"),
                exclusiveNodes = listOf("gpu"),
            )
        val mine =
            base.copy(
                access =
                    base.access.copy(
                        projects = listOf("p1"),
                        mcpTools = mapOf("m" to listOf("t1", "t2")),
                    ),
                plugins = listOf("prod", "staging"),
            )
        val theirs =
            base.copy(
                revision = "r2",
                access = base.access.copy(projects = listOf("p1", "p2", "p3")),
                plugins = listOf("prod", "bot"),
                exclusiveNodes = emptyList(),
            )
        val rebased = rebaseAgent(base, mine, theirs, mcpIds = setOf("m"))
        // I removed p2; they added p3.
        assertEquals(listOf("p1", "p3"), rebased.access.projects)
        assertEquals(listOf("prod", "bot", "staging"), rebased.plugins)
        assertEquals(emptyList<String>(), rebased.exclusiveNodes)
        assertEquals(mapOf("m" to listOf("t1", "t2")), rebased.access.mcpTools)
        // Tool choices of a connection deleted meanwhile are dropped.
        assertTrue(rebaseAgent(base, mine, theirs, mcpIds = emptySet()).access.mcpTools.isEmpty())
    }

    @Test
    fun olderInstallationsAreRecognizedByTheMissingMarkerNotTheRevision() {
        val current = agent.copy(grantsOnAgent = true)
        assertFalse(olderInstallation(listOf(current, agent)))
        // A rolled-back release keeps revisions but no longer marks its agents.
        assertTrue(olderInstallation(listOf(agent, agent.copy(id = "other"))))
        assertFalse(olderInstallation(emptyList()))
        // Decoded, never sent.
        val decoded =
            wireJson.decodeFromString<Agent>(
                """{"id":"a","name":"A","revision":"r1","grantsOnAgent":true}"""
            )
        assertTrue(decoded.grantsOnAgent)
        assertFalse("grantsOnAgent" in agentSaveBody(decoded))
    }

    @Test
    fun anEditedInstanceKeepsItsGrantsUnlessTheInstallationIsKnownCurrent() {
        val account = catalog.connections.first { it.id == "prod" }.copy(agentIds = listOf("agent"))
        val access = catalog.packages.first { it.id == "onepassword" }.manifest!!
        val current = listOf(agent.copy(grantsOnAgent = true))
        assertNull(account.agentIdsToKeep(current, access))
        assertEquals(listOf("agent"), account.agentIdsToKeep(listOf(agent), access))
        // Agents not loaded yet: unknown, so the grants are kept.
        assertEquals(listOf("agent"), account.agentIdsToKeep(emptyList(), access))
        // Nothing to keep without grants or without agent access.
        assertNull(account.copy(agentIds = emptyList()).agentIdsToKeep(emptyList(), access))
        assertNull(account.agentIdsToKeep(emptyList(), PluginManifest(id = "discord")))
    }

    @Test
    fun anOlderOrUnknownInstallationNeverReceivesLiveGrantsOrARevision() {
        val granted = agent.copy(plugins = listOf("prod"), exclusiveNodes = listOf("gpu"))
        val current = agentSaveBody(granted)
        assertTrue("plugins" in current && "exclusiveNodes" in current && "revision" in current)
        val older = agentSaveBody(granted, liveGrants = false)
        assertFalse("plugins" in older)
        assertFalse("exclusiveNodes" in older)
        assertFalse("revision" in older)
        assertEquals("Release engineer", older["name"]!!.jsonPrimitive.content)
    }

    @Test
    fun grantsSentBackAreReadAgainJustBeforeSaving() = runBlocking {
        val account = catalog.connections.first { it.id == "prod" }.copy(agentIds = listOf("agent"))
        val access = catalog.packages.first { it.id == "onepassword" }.manifest!!
        // Revoked meanwhile: nothing to send back.
        assertNull(account.agentIdsToSend(emptyList(), access) { emptyList() })
        assertEquals(
            listOf("other"),
            account.agentIdsToSend(emptyList(), access) { listOf("other") },
        )
        assertNull(account.agentIdsToSend(emptyList(), access) { null })
        // Always read again, whatever the editor last saw: a grant added meanwhile is kept.
        val none = account.copy(agentIds = emptyList())
        assertEquals(listOf("late"), none.agentIdsToSend(listOf(agent), access) { listOf("late") })
        assertNull(none.agentIdsToSend(listOf(agent), access) { emptyList() })
        // A current installation keeps them itself: no extra read.
        var read = false
        assertNull(
            account.agentIdsToSend(listOf(agent.copy(grantsOnAgent = true)), access) {
                read = true
                listOf("agent")
            }
        )
        assertFalse(read)
    }

    @Test
    fun olderInstallationsCheckAndReviewTheGrantsAsShown() {
        val shownCatalog =
            catalog.copy(
                connections =
                    catalog.connections.map {
                        if (it.id == "prod") it.copy(agentIds = listOf("agent")) else it
                    }
            )
        val shownNodes = nodes.map {
            if (it.id == "mac") it.copy(exclusiveAgents = listOf("agent")) else it
        }
        val shown = agent.withResourceGrants(shownCatalog.agentDirectory(), shownNodes)
        assertEquals(listOf("prod"), shown.plugins)
        // The exclusive reservation on a machine it cannot run on is reported from the resources.
        assertTrue(
            agentIssues(
                    shown,
                    resources.copy(nodes = shownNodes, catalog = shownCatalog.agentDirectory()),
                )
                .any {
                    it.section == AgentSection.Nodes
                }
        )

        // Live-grant differences are not reviewed: they are never saved from the agent there.
        val edited = agent.copy(plugins = listOf("prod"), exclusiveNodes = listOf("gpu"))
        val legacyLines =
            agentReview(reviewBaseline(agent, edited, legacy = true), edited, resources, null)
        assertTrue(legacyLines.isEmpty())
        val currentLines =
            agentReview(reviewBaseline(agent, edited, legacy = false), edited, resources, null)
        assertEquals(2, currentLines.size)
    }

    @Test
    fun aBackendSwitchOnEitherSideKeepsThatSidesNodesWhole() {
        val base = agent.withExclusive("gpu", true)
        // I switched to native and chose desk; they added mac meanwhile.
        val mine = base.withBackend(native = true).withNodeRun("desk", true)
        val theirs =
            base.copy(
                revision = "r2",
                access = base.access.copy(nodes = base.access.nodes!! + "mac"),
            )
        val rebased = rebaseAgent(base, mine, theirs)
        assertEquals(NATIVE_BACKEND, rebased.access.backend)
        assertEquals(listOf("desk"), rebased.access.nodes)
        assertEquals(emptyList<String>(), rebased.exclusiveNodes)

        // They switched; I only added a VM node: their native nodes win, mine is not merged in.
        val theirsNative = base.withBackend(native = true).withNodeRun("desk", true)
        val mineVm = base.withNodeRun("mac", true)
        val kept = rebaseAgent(base, mineVm, theirsNative.copy(revision = "r2"))
        assertEquals(NATIVE_BACKEND, kept.access.backend)
        assertEquals(listOf("desk"), kept.access.nodes)
    }

    @Test
    fun cardsAndReviewsNameTheBackendAndOnlyEffectiveExclusivity() {
        val native = agent.withBackend(native = true).withNodeRun("desk", true)
        assertEquals(
            listOf("exécution native"),
            agentReductions(native, native.withBackend(native = false), resources).filter {
                it == "exécution native"
            },
        )
        assertEquals("2 projets · 1 node · Exécution native", agentAccessSummary(native))

        val granted = agent.copy(plugins = listOf("prod", "staging")).withExclusive("mac", true)
        // Without the catalog or machines: counts, and no exclusivity that may not apply.
        assertEquals("2 projets · 2 nodes · 2 plugins", agentAccessSummary(granted))
        // An older installation records them on its resources: unknown without the catalog.
        assertEquals("2 projets · 2 nodes", agentAccessSummary(granted, older = true))
        assertEquals(
            "2 projets · 2 nodes · Exclusivité",
            agentAccessSummary(agent.withExclusive("gpu", true), nodes = nodes),
        )
        // With them: package names, and exclusivity only where it applies (mac cannot run it).
        assertEquals(
            "2 projets · 2 nodes · 1Password",
            agentAccessSummary(granted, catalog.agentDirectory(), nodes),
        )
    }

    @Test
    fun toolChoicesOfDeletedConnectionsAreNeverSent() {
        val tools = mapOf("kept" to listOf("a"), "gone" to listOf("b"))
        val all = agent.copy(access = agent.access.copy(mcps = null, mcpTools = tools))
        val sent = agentSaveBody(all, mcpIds = setOf("kept"))["access"]!!.jsonObject["mcpTools"]!!
        assertEquals(setOf("kept"), sent.jsonObject.keys)
        assertEquals(
            setOf("kept", "gone"),
            agentSaveBody(all)["access"]!!.jsonObject["mcpTools"]!!.jsonObject.keys,
        )
    }

    @Test
    fun runPermissionsOnUnusableNodesAreReportedAndNativeSupportsAllNodes() {
        val desk =
            ExecutionNode(
                "desk",
                "desk-native",
                status = "online",
                capabilities = NodeCapabilities(backend = NATIVE_BACKEND),
            )
        val holder = agent.copy(access = agent.access.copy(nodes = listOf("gpu", "desk", "gone")))
        val unusable = holder.unusableNodes(nodes + desk)
        assertEquals(listOf("desk", "gone"), unusable.map { it.id })
        assertEquals(listOf(true, false), unusable.map { it.otherBackend })
        val issues = agentIssues(holder, resources.copy(nodes = nodes + desk))
        assertTrue(issues.any { "« desk-native » utilise un autre mode d’exécution" in it.text })
        assertTrue(issues.any { it.text == "Une node autorisée n’existe plus ou a été révoquée." })

        val native = agent.withBackend(native = true)
        assertFalse(AgentSection.Nodes in agentErrors(AgentDraft(native)))
        val allNative = native.copy(access = native.access.withAllNodes(true, nodes + desk))
        assertNull(allNative.access.nodes)
        assertFalse(AgentSection.Nodes in agentErrors(AgentDraft(allNative)))
        assertFalse(AgentSection.Nodes in agentErrors(AgentDraft(native.withNodeRun("desk", true))))
    }

    @Test
    fun toolRestrictionsWaitInTheDraftAndOnlyGrantedOnesAreSaved() {
        val restricted =
            agent.copy(
                access = agent.access.copy(mcps = listOf("m"), mcpTools = mapOf("m" to listOf("t")))
            )
        // The connection is removed: its restriction stays in the draft but is not saved.
        val removed = restricted.copy(access = restricted.access.copy(mcps = emptyList()))
        assertEquals(mapOf("m" to listOf("t")), removed.access.mcpTools)
        assertEquals(0, agentSaveBody(removed)["access"]!!.jsonObject["mcpTools"]!!.jsonObject.size)
        // Granted again, it comes back restricted: nothing to review, no tool change.
        val again = removed.copy(access = removed.access.copy(mcps = listOf("m")))
        assertEquals(
            listOf("t"),
            agentSaveBody(again)["access"]!!
                .jsonObject["mcpTools"]!!
                .jsonObject["m"]!!
                .jsonArray
                .map { it.jsonPrimitive.content },
        )
        assertTrue(agentChanges(restricted, AgentDraft(again)).isEmpty())
    }

    @Test
    fun nativeExecutionUsesOnlyNativeNodesAndIsReviewed() {
        val desk =
            ExecutionNode(
                "desk",
                "desk-native",
                status = "online",
                capabilities = NodeCapabilities(backend = NATIVE_BACKEND),
            )
        val all = nodes + desk
        val reserved = agent.withExclusive("gpu", true)

        // New agents and existing ones default to a VM.
        assertEquals(VM_BACKEND, newAgent().access.backend)
        assertEquals(listOf(LOCAL_NODE_ID, "gpu", "mac"), reserved.nodeChoices(all).map { it.id })

        // Switching starts again from no node and no exclusive reservation.
        val native = reserved.withBackend(native = true)
        assertEquals(NATIVE_BACKEND, native.access.backend)
        assertEquals(emptyList<String>(), native.access.nodes)
        assertEquals(emptyList<String>(), native.exclusiveNodes)
        assertEquals(listOf("desk"), native.nodeChoices(all).map { it.id })
        assertEquals(native, native.withBackend(native = true))
        // An older installation keeps reservations on its nodes: they stay as shown.
        assertEquals(
            listOf("gpu"),
            reserved.withBackend(native = true, readOnlyGrants = true).exclusiveNodes,
        )
        assertEquals(
            listOf("gpu"),
            reserved.withNodeRun("gpu", false, readOnlyGrants = true).exclusiveNodes,
        )
        assertEquals(emptyList<String>(), reserved.withNodeRun("gpu", false).exclusiveNodes)

        val running = native.withNodeRun("desk", true)
        assertEquals("Exécution native · 1 sur 1", nodesSummary(running, all))
        assertEquals(
            listOf(AgentSection.Nodes),
            agentChanges(agent, AgentDraft(running)).distinct(),
        )
        val lines = agentReview(agent, running, resources.copy(nodes = all), null)
        assertEquals(ReviewKind.Power, lines.first().kind)
        assertTrue(lines.first().text.startsWith("Ajout : exécution native sur l’hôte."))
        assertTrue(needsReview(lines, null))

        // Back to a VM: no power added, the native machines are no longer offered.
        val vm = running.withBackend(native = false)
        assertEquals(VM_BACKEND, vm.access.backend)
        assertEquals(emptyList<String>(), vm.access.nodes)
        assertTrue(agentReview(running, vm, resources, null).none { it.kind == ReviewKind.Power })

        // A reload keeps a backend switch made on top of the newer version.
        assertEquals(
            NATIVE_BACKEND,
            rebaseAgent(agent, running, agent.copy(revision = "r2")).access.backend,
        )
    }

    @Test
    fun toolChoicesOfConnectionsNoLongerGrantedAreDropped() {
        val tools = mapOf("a" to listOf("x"), "b" to listOf("y"))
        val all = agent.access.copy(mcps = null, mcpTools = tools)
        assertEquals(tools, all.withGrantedTools().mcpTools)
        val chosen = all.copy(mcps = listOf("a"))
        assertEquals(mapOf("a" to listOf("x")), chosen.withGrantedTools().mcpTools)

        val body = agentSaveBody(agent.copy(access = all.copy(mcps = emptyList())))
        assertEquals(0, body["access"]!!.jsonObject["mcpTools"]!!.jsonObject.size)
    }

    @Test
    fun aMissingSkillIsReportedOnceSkillsHaveLoadedEvenWhenNoneExist() {
        val holder = agent.copy(access = agent.access.copy(skills = listOf("global/gone")))
        val none = resources.copy(skills = emptyList())
        assertEquals(
            listOf("La skill « gone » n’existe plus."),
            agentIssues(holder, none).map { it.text },
        )
        assertTrue(agentIssues(holder, none.copy(skillsLoaded = false)).isEmpty())
    }

    @Test
    fun reviewNamesNewToolChoicesAndOtherwiseInterruptedWork() {
        // Tools chosen on a connection granted in the same edit are not a restriction.
        val limited = agent.copy(access = agent.access.copy(mcps = listOf("a")))
        val added =
            limited.copy(
                access =
                    limited.access.copy(mcps = listOf("a", "b"), mcpTools = mapOf("b" to listOf()))
            )
        assertEquals(emptyList<String>(), agentReductions(limited, added, resources))
        val narrowed =
            limited.copy(access = limited.access.copy(mcpTools = mapOf("a" to listOf("x"))))
        assertEquals(listOf("outils MCP restreints"), agentReductions(limited, narrowed, resources))

        // Work the server says stops is reported even when nothing listed is removed.
        val runs = AgentPreview(AgentPreviewRuns(queued = 2, running = 1))
        val renamed = agent.copy(name = "Renamed")
        val line = agentReview(agent, renamed, resources, runs).single()
        assertEquals(ReviewKind.Reduction, line.kind)
        assertEquals(
            "Accès modifié. 2 exécutions en file et 1 conversation en cours ne pourront pas continuer avec les anciens accès.",
            line.text,
        )
        assertTrue(needsReview(listOf(line), runs))
    }

    @Test
    fun nodesAreNeverRestrictedWithoutTheMachineList() {
        val everywhere = agent.access.copy(nodes = null)
        assertEquals(everywhere, everywhere.withAllNodes(false, emptyList()))
        assertEquals(
            listOf(LOCAL_NODE_ID, "gpu", "mac"),
            everywhere.withAllNodes(false, nodes).nodes,
        )
    }

    @Test
    fun anOlderInstallationWithoutPreviewIsReviewedLocally() = runBlocking {
        assertNull(optionalPreview<AgentPreview> { throw ApiException(404, "Not found") })
        assertNull(optionalPreview<AgentPreview> { throw ApiException(405, "Not allowed") })
        assertEquals(AgentPreview(), optionalPreview { AgentPreview() })
        val refused = runCatching {
            optionalPreview<AgentPreview> { throw ApiException(400, "Refusé") }
        }
        assertEquals(400, (refused.exceptionOrNull() as ApiException).status)
    }

    @Test
    fun nodeGrantsOnlyAddAndFallBackToTheMergedListOnOlderInstallations() = runBlocking {
        val sent = mutableListOf<JsonObject>()
        grantNodeAgents(listOf("agent", "agent"), setOf("other")) { sent += it }
        assertEquals(1, sent.size)
        assertEquals(
            listOf("agent"),
            sent.single()["agentIds"]!!.jsonArray.map { it.jsonPrimitive.content },
        )
        assertEquals(true, sent.single()["add"]!!.jsonPrimitive.content.toBoolean())

        // An installation that predates `add` rejects it: the full list is sent once without it.
        sent.clear()
        grantNodeAgents(listOf("agent"), setOf("other")) { body ->
            sent += body
            // What an older server answers when decoding the unknown field fails.
            if ("add" in body) throw ApiException(400, "Invalid node request.")
        }
        assertEquals(2, sent.size)
        assertFalse("add" in sent.last())
        assertEquals(
            setOf("other", "agent"),
            sent.last()["agentIds"]!!.jsonArray.map { it.jsonPrimitive.content }.toSet(),
        )

        // A serde-style unknown-field message is recognized too.
        sent.clear()
        grantNodeAgents(listOf("agent"), setOf("other")) { body ->
            sent += body
            if ("add" in body) throw ApiException(400, "unknown field `add`, expected `agentIds`")
        }
        assertEquals(2, sent.size)

        // Other failures, including another 400, are reported without replacing the list.
        for (error in listOf(ApiException(500, "Erreur"), ApiException(400, "Invalid agent id."))) {
            sent.clear()
            val failed = runCatching {
                grantNodeAgents(listOf("agent"), setOf("other")) { body ->
                    sent += body
                    throw error
                }
            }
            assertEquals(error, failed.exceptionOrNull())
            assertEquals(1, sent.size)
        }

        // Nothing new chosen: no request at all.
        sent.clear()
        grantNodeAgents(listOf("other"), setOf("other")) { sent += it }
        grantNodeAgents(emptyList(), setOf("other")) { sent += it }
        assertTrue(sent.isEmpty())
    }

    @Test
    fun reviewDoesNotOpenForAnEarlierSuspensionAlone() {
        val reduction = ReviewLine(ReviewKind.Reduction, "Retrait : projet « SheetOM ».")
        assertFalse(needsReview(listOf(reduction), null))

        val earlier =
            AgentPreview(
                delegations =
                    listOf(AgentDelegationEffect("bot", suspended = true, alreadySuspended = true))
            )
        assertFalse(needsReview(emptyList(), earlier))
        val now = AgentPreview(delegations = listOf(AgentDelegationEffect("bot", suspended = true)))
        assertTrue(needsReview(emptyList(), now))
    }

    @Test
    fun reorderedToolChoicesAreNotAChange() {
        val chosen =
            agent.copy(access = agent.access.copy(mcpTools = mapOf("m" to listOf("a", "b"))))
        val reordered =
            AgentDraft(
                chosen.copy(access = chosen.access.copy(mcpTools = mapOf("m" to listOf("b", "a"))))
            )
        assertTrue(agentChanges(chosen, reordered).isEmpty())
        val narrowed =
            AgentDraft(
                chosen.copy(access = chosen.access.copy(mcpTools = mapOf("m" to listOf("a"))))
            )
        assertEquals(listOf(AgentSection.Mcp), agentChanges(chosen, narrowed))
    }

    @Test
    fun issuesFlagMissingProjectsAndDisabledConnectionsGrantedThroughAll() {
        val mcps =
            listOf(
                Mcp(id = "on", name = "Linear"),
                Mcp(id = "off", name = "Sentry", enabled = false),
            )
        val gone =
            agent.copy(access = agent.access.copy(projects = listOf("p1", "gone"), mcps = null))
        val issues = agentIssues(gone, resources.copy(mcps = mcps))
        assertEquals(listOf(AgentSection.Projects, AgentSection.Mcp), issues.map { it.section })
        assertEquals("Un projet sélectionné n’existe plus.", issues[0].text)
        assertTrue("« Sentry » est désactivée" in issues[1].text)

        // The main agent always has every project and connection: nothing to check there.
        assertTrue(
            agentIssues(gone.copy(id = MAIN_AGENT_ID), resources.copy(mcps = mcps)).isEmpty()
        )
        // A connection the agent does not hold is not its problem.
        val limited = gone.copy(access = gone.access.copy(projects = null, mcps = listOf("on")))
        assertTrue(agentIssues(limited, resources.copy(mcps = mcps)).isEmpty())
    }

    @Test
    fun issuesFlagDisabledAccountsAndMissingSkills() {
        val withGrants =
            agent.copy(
                access = agent.access.copy(skills = listOf("global/gone")),
                plugins = listOf("staging"),
            )
        val issues =
            agentIssues(
                withGrants,
                resources.copy(skills = listOf(Skill("review", scope = "global"))),
            )
        assertEquals(listOf(AgentSection.Skills, AgentSection.Plugins), issues.map { it.section })
        assertEquals("2 points à vérifier", issuesLabel(issues.size))
    }

    @Test
    fun reloadKeepsMyEditsOnTopOfTheNewestVersion() {
        val mine = agent.copy(name = "Mine", plugins = listOf("prod"))
        val theirs =
            agent.copy(
                description = "Theirs",
                revision = "r2",
                access = agent.access.copy(sandbox = "read-only"),
                exclusiveNodes = listOf("gpu"),
            )
        val rebased = rebaseAgent(agent, mine, theirs)
        assertEquals("Mine", rebased.name)
        assertEquals("Theirs", rebased.description)
        assertEquals("read-only", rebased.access.sandbox)
        assertEquals(listOf("prod"), rebased.plugins)
        assertEquals(listOf("gpu"), rebased.exclusiveNodes)
        assertEquals("r2", rebased.revision)
    }

    @Test
    fun reloadTakesTheNewerTimeLimitUnlessIEditedIt() {
        val base = agent.copy(timeoutMinutes = 30)
        val theirs = base.copy(timeoutMinutes = 90, revision = "r2")

        // Untouched limit: the newer value replaces the field, so saving does not restore 30.
        val untouched = AgentDraft(base.copy(name = "Mine")).rebased(base, theirs)
        assertEquals("90", untouched.timeout)
        assertEquals(90, untouched.resolved().timeoutMinutes)
        assertEquals("Mine", untouched.agent.name)
        assertEquals(
            listOf(AgentSection.Identity),
            agentChanges(theirs, untouched),
        )

        // Edited limit: my value is kept on top of the newer version.
        val edited = AgentDraft(base, timeout = "45").rebased(base, theirs)
        assertEquals("45", edited.timeout)
        assertEquals(45, edited.resolved().timeoutMinutes)
    }

    @Test
    fun heldGrantsOnUngrantablePackagesAreShownAndReported() {
        val ungrantable =
            catalog.copy(
                packages =
                    catalog.packages +
                        listOf(
                            // Uploaded, never approved: no version runs.
                            PluginEntry(
                                "vault",
                                status = "needs_approval",
                                manifest =
                                    PluginManifest(
                                        id = "vault",
                                        name = "Vault",
                                        agentAccess = PluginAgentAccess("Lecture"),
                                    ),
                            ),
                            PluginEntry("broken", status = "invalid"),
                        ),
                connections =
                    catalog.connections +
                        listOf(
                            PluginConnection("v1", "vault", "Coffre"),
                            PluginConnection("b1", "broken", "Cassé"),
                        ),
            )
        // The Discord package gives agents nothing; the others no longer run.
        val holder = agent.copy(plugins = listOf("prod", "bot", "v1", "b1"))
        assertEquals(
            listOf("bot", "v1", "b1"),
            ungrantable.agentDirectory().unavailableGrants(holder.plugins).map { it.id },
        )
        assertEquals(
            listOf("onepassword"),
            ungrantable.agentDirectory().agentGrantable().map { it.first.id },
        )
        val issues = agentIssues(holder, resources.copy(catalog = ungrantable.agentDirectory()))
        assertEquals(3, issues.size)
        assertTrue(issues.all { it.section == AgentSection.Plugins })
        assertTrue(issues.any { "« Coffre » n’est plus disponible" in it.text })
        assertEquals("3 points à vérifier", issuesLabel(issues.size))

        // An instance it does not hold is not reported.
        assertTrue(ungrantable.agentDirectory().unavailableGrants(listOf("prod")).isEmpty())
    }

    @Test
    fun aSavedProfileIsNotSentAgainButLaterEditsAre() {
        val draft = AgentDraft(agent).copy(agent = agent.copy(name = "Releases"))
        val saved = draft.resolved().copy(revision = "r2")

        // Only its reload failed: saving again sends nothing new, from the new revision.
        val retry = AgentDraft(saved)
        assertFalse(profileChanged(saved, retry.resolved()))
        assertTrue(agentChanges(saved, retry).isEmpty())
        assertEquals("r2", agentSaveBody(retry.resolved())["revision"]!!.jsonPrimitive.content)

        // Any edit made afterwards is a profile change again.
        val edited = retry.agent.copy(description = "Later")
        assertTrue(profileChanged(saved, edited))
    }

    @Test
    fun aNewAgentStartsWithoutAccessWhileExistingAgentsKeepServerDefaults() {
        val created = newAgent()
        val access = agentSaveBody(created)["access"]!!.jsonObject
        assertEquals(0, access["projects"]!!.jsonArray.size)
        assertEquals(0, access["skills"]!!.jsonArray.size)
        assertEquals(0, access["mcps"]!!.jsonArray.size)
        assertEquals(0, access["mcpTools"]!!.jsonObject.size)
        val body = agentSaveBody(created)
        assertEquals(0, body["exclusiveNodes"]!!.jsonArray.size)
        assertEquals(0, body["plugins"]!!.jsonArray.size)
        assertEquals(LOCAL_NODE_ID, access["nodes"]!!.jsonArray.single().jsonPrimitive.content)
        // GitHub is a plugin instance granted like any other: no access field of its own.
        assertFalse("github" in access)
        assertFalse(access["installation"]!!.jsonPrimitive.content.toBoolean())
        assertEquals("yolo", access["sandbox"]!!.jsonPrimitive.content)

        // A saved agent without access fields still decodes to the server defaults.
        val existing = wireJson.decodeFromString<Agent>("""{"id":"a","name":"Old"}""")
        assertNull(existing.access.projects)
        // An older installation's `github` flag is ignored.
        val older =
            wireJson.decodeFromString<Agent>(
                """{"id":"a","name":"Old","access":{"github":true,"projects":[]}}"""
            )
        assertFalse("github" in agentSaveBody(older)["access"]!!.jsonObject)
    }

    @Test
    fun emptyGrantsOmittedByTheServerAreNotUnsavedChanges() {
        val omitted =
            wireJson.decodeFromString<Agent>(
                """{"id":"agent","name":"Release","revision":"r1","access":{"nodes":["$LOCAL_NODE_ID"]}}"""
            )
        val explicit =
            wireJson.decodeFromString<Agent>(
                """{"id":"agent","name":"Release","revision":"r1","access":{"nodes":["$LOCAL_NODE_ID"]},"exclusiveNodes":[],"plugins":[]}"""
            )
        assertEquals(explicit, omitted)
        assertTrue(agentChanges(omitted, AgentDraft(explicit)).isEmpty())
        assertFalse(profileChanged(omitted, explicit))
    }

    @Test
    fun resourceRulesRemainVisibleWithoutWholeInstanceAccess() {
        val scoped = agent.copy(plugins = emptyList())
        val catalog = PluginCatalog(resourceGrantCounts = mapOf(scoped.id to 2))
        assertEquals("2 règles sur les ressources", pluginSummary(scoped, catalog.agentDirectory()))
        assertTrue(scoped.plugins.isEmpty())
        assertTrue(
            agentAccessSummary(scoped, catalog.agentDirectory())
                .contains("2 règles sur les ressources")
        )
        assertEquals(
            "",
            pluginSummary(scoped, catalog.copy(resourceGrantCounts = emptyMap()).agentDirectory()),
        )
    }
}
