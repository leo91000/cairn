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
    private val resources = AgentResources(projects, nodes = nodes, catalog = catalog)

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
        assertEquals("", pluginSummary(agent, catalog))
        assertEquals(
            "1Password : 2 comptes",
            pluginSummary(agent.copy(plugins = listOf("prod", "staging")), catalog),
        )
        assertEquals("review · Global", skillLabel(Skill("review", scope = "global"), projects))
        assertEquals("lint · cairn", skillKeyLabel("p1/lint", projects))
        assertEquals("2 projets · 2 nodes", agentAccessSummary(agent))
        assertEquals("Limite : 120 min", timeoutSummary(120))
    }

    @Test
    fun changesAreCountedPerSettingAndTheTokenGoesWithThePage() {
        val draft = AgentDraft(agent)
        assertTrue(agentChanges(agent, draft, tokenSaved = false, newToken = "").isEmpty())

        val edited =
            draft.copy(
                agent =
                    agent.copy(
                        name = "Releases",
                        plugins = listOf("prod"),
                        exclusiveNodes = listOf("gpu"),
                    ),
                github = GithubMode.Dedicated.value,
            )
        val changes = agentChanges(agent, edited, tokenSaved = false, newToken = "ghp_x")
        assertEquals(
            listOf(
                AgentSection.Identity,
                AgentSection.Github,
                AgentSection.Plugins,
                AgentSection.Nodes,
            ),
            changes,
        )
        assertEquals("4 modifications non enregistrées", changesLabel(changes.size))
        assertEquals("ghp_x", edited.tokenChange(false, "ghp_x"))

        // Dedicated without any token cannot be saved; "No GitHub" removes a saved token.
        assertEquals(
            setOf(AgentSection.Github),
            agentErrors(edited, tokenSaved = false, newToken = "").keys,
        )
        assertEquals("", draft.withGithub(GithubMode.None, tokenSaved = true).tokenChange(true, ""))
    }

    @Test
    fun invalidDraftsPointToTheirSection() {
        val errors = agentErrors(AgentDraft(agent.copy(name = " "), timeout = "900"), false, "")
        assertEquals(setOf(AgentSection.Identity, AgentSection.Advanced), errors.keys)
        assertEquals(AgentSection.Nodes, sectionOfError("Exclusive agent missing."))
        assertEquals(
            AgentSection.Plugins,
            sectionOfError("A selected plugin instance no longer exists."),
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
    fun restrictingProjectsTurnsSharedGithubOffAndSaysSo() {
        val shared = agent.copy(access = agent.access.copy(projects = null, github = true))
        // Choosing starts with no project selected, as on the web.
        val restricted = shared.access.withAllProjects(false)
        assertEquals(emptyList<String>(), restricted.projects)
        assertFalse(restricted.github)
        val lines = agentReview(shared, shared.copy(access = restricted), resources, null)
        assertTrue(lines.any { it.text.startsWith("Connexion GitHub partagée désactivée") })
    }

    @Test
    fun restrictingProjectsMovesASharedAgentToItsSavedTokenAndSaysSo() {
        val shared = agent.copy(access = agent.access.copy(projects = null, github = true))
        val draft = AgentDraft(shared).withAllProjects(false, tokenSaved = true)

        // The saved token, made for this agent, replaces the shared connection; it is kept.
        assertEquals(GithubMode.Dedicated, draft.githubMode(tokenSaved = true))
        assertFalse(draft.resolved(tokenSaved = true).access.github)
        assertNull(draft.tokenChange(tokenSaved = true, newToken = ""))
        assertTrue(draft.switchedToSavedToken(tokenSaved = true))
        assertEquals(
            listOf(AgentSection.Projects, AgentSection.Github),
            agentChanges(shared, draft, tokenSaved = true, newToken = ""),
        )
        val lines = agentReview(shared, draft.resolved(true), resources, null, savedToken = true)
        assertTrue(
            lines.any {
                it.kind == ReviewKind.Reduction &&
                    it.text ==
                        "Connexion GitHub partagée désactivée : l’agent utilisera son jeton dédié enregistré."
            }
        )
        // The choice survives a reload; an explicit choice clears the notice.
        assertTrue(draft.rebased(shared, shared).switchedToSavedToken(tokenSaved = true))
        assertFalse(
            draft
                .withGithub(GithubMode.Dedicated, tokenSaved = true)
                .switchedToSavedToken(tokenSaved = true)
        )

        // Without a saved token GitHub is switched off, with the existing explanation.
        val none = AgentDraft(shared).withAllProjects(false, tokenSaved = false)
        assertEquals(GithubMode.None, none.githubMode(tokenSaved = false))
        assertNull(none.tokenChange(tokenSaved = false, newToken = ""))
        assertFalse(none.switchedToSavedToken(tokenSaved = false))
        assertTrue(
            agentReview(shared, none.resolved(false), resources, null).any {
                it.text ==
                    "Connexion GitHub partagée désactivée : les projets choisis exigent un jeton dédié."
            }
        )

        // An agent not using the shared connection keeps its GitHub choice.
        val dedicated =
            AgentDraft(agent.copy(access = agent.access.copy(projects = null, github = false)))
        val kept = dedicated.withAllProjects(false, tokenSaved = true)
        assertEquals(GithubMode.Dedicated, kept.githubMode(tokenSaved = true))
        assertFalse(kept.sharedTurnedOff)
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
        val shown = agent.withResourceGrants(shownCatalog, shownNodes)
        assertEquals(listOf("prod"), shown.plugins)
        // The exclusive reservation on a machine it cannot run on is reported from the resources.
        assertTrue(
            agentIssues(shown, resources.copy(nodes = shownNodes, catalog = shownCatalog)).any {
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
        assertEquals(
            "2 projets · 2 nodes · Exclusivité",
            agentAccessSummary(agent.withExclusive("gpu", true), nodes = nodes),
        )
        // With them: package names, and exclusivity only where it applies (mac cannot run it).
        assertEquals("2 projets · 2 nodes · 1Password", agentAccessSummary(granted, catalog, nodes))
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
    fun anExplicitDedicatedChoiceDropsTheAutomaticSwitchLine() {
        val shared = agent.copy(access = agent.access.copy(projects = null, github = true))
        val switched = AgentDraft(shared).withAllProjects(false, tokenSaved = true)
        assertTrue(switched.switchedToSavedToken(tokenSaved = true))
        val chosen = switched.withGithub(GithubMode.Dedicated, tokenSaved = true)
        assertFalse(chosen.switchedToSavedToken(tokenSaved = true))
        val lines =
            agentReview(
                shared,
                chosen.resolved(true),
                resources,
                null,
                savedToken = chosen.switchedToSavedToken(true),
            )
        assertTrue(lines.none { "utilisera son jeton dédié enregistré" in it.text })
    }

    @Test
    fun runPermissionsOnUnusableNodesAreReportedAndNativeNeedsANode() {
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
        assertEquals(
            "Choisissez au moins une node native : l’exécution native ne s’applique jamais à toutes les nodes.",
            agentErrors(AgentDraft(native), false, "")[AgentSection.Nodes],
        )
        assertFalse(
            AgentSection.Nodes in
                agentErrors(AgentDraft(native.withNodeRun("desk", true)), false, "")
        )
    }

    @Test
    fun anUnknownTokenStillOpensTheReviewWhenSharedGithubIsTurnedOff() {
        val shared = agent.copy(access = agent.access.copy(projects = null, github = true))
        val restricted = AgentDraft(shared).withAllProjects(false, tokenSaved = false)
        val lines =
            agentReview(shared, restricted.resolved(false), resources, null, tokenUnknown = true)
        val line = lines.single { it.text.startsWith("Connexion GitHub partagée désactivée") }
        assertEquals(
            "Connexion GitHub partagée désactivée : l’agent utilisera son jeton dédié enregistré s’il en existe un.",
            line.text,
        )
        assertTrue(needsReview(lines, null))
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
        assertTrue(agentChanges(restricted, AgentDraft(again), false, "").isEmpty())
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
            agentChanges(agent, AgentDraft(running), false, "").distinct(),
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
    fun aSavedTokenIsOnlyRemovedByAnExplicitChoice() {
        val shared = agent.copy(access = agent.access.copy(projects = null, github = true))

        // Scenario A: projects are restricted before the token's state has loaded.
        val early = AgentDraft(shared).withAllProjects(false, tokenSaved = false)
        // « Pas de GitHub » is derived, not recorded as the owner's choice.
        assertNull(early.github)
        assertEquals(GithubMode.None, early.githubMode(tokenSaved = false))
        // The token then turns out to be saved: the agent uses it, nothing is deleted.
        assertEquals(GithubMode.Dedicated, early.githubMode(tokenSaved = true))
        assertNull(early.tokenChange(tokenSaved = true, newToken = ""))
        assertTrue(early.switchedToSavedToken(tokenSaved = true))

        // Scenario B: « Pas de GitHub » chosen while no token was saved, then a reload shows
        // one saved elsewhere: it is kept and used.
        val dedicated = agent.copy(access = agent.access.copy(github = false))
        val chosen = AgentDraft(dedicated).withGithub(GithubMode.None, tokenSaved = false)
        val reloaded = chosen.rebased(dedicated, dedicated.copy(revision = "r2"))
        assertNull(reloaded.tokenChange(tokenSaved = true, newToken = ""))
        assertEquals(GithubMode.Dedicated, reloaded.githubMode(tokenSaved = true))

        // The explicit removal of a known token.
        val removed = AgentDraft(dedicated).withGithub(GithubMode.None, tokenSaved = true)
        assertTrue(removed.removeToken)
        assertEquals(GithubMode.None, removed.githubMode(tokenSaved = true))
        assertEquals("", removed.tokenChange(tokenSaved = true, newToken = ""))
        // Picking another option cancels it.
        assertNull(
            removed.withGithub(GithubMode.Dedicated, tokenSaved = true).tokenChange(true, "")
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
    fun reviewNamesTheRemovedTokenNewToolChoicesAndOtherwiseInterruptedWork() {
        val dedicated = AgentDraft(agent).withGithub(GithubMode.None, tokenSaved = true)
        val tokenRemoved = dedicated.tokenChange(tokenSaved = true, newToken = "") == ""
        assertEquals(
            "Retrait : jeton GitHub dédié.",
            agentReview(agent, agent, resources, null, tokenRemoved).single().text,
        )

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
    fun reviewOpensForTheTokenSwitchButNotForAnEarlierSuspensionAlone() {
        val switched =
            ReviewLine(ReviewKind.Reduction, "Connexion GitHub partagée désactivée", true)
        assertTrue(needsReview(listOf(switched), null))
        assertFalse(needsReview(listOf(switched.copy(opensReview = false)), null))

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
        assertTrue(agentChanges(chosen, reordered, false, "").isEmpty())
        val narrowed =
            AgentDraft(
                chosen.copy(access = chosen.access.copy(mcpTools = mapOf("m" to listOf("a"))))
            )
        assertEquals(listOf(AgentSection.Mcp), agentChanges(chosen, narrowed, false, ""))
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
        assertEquals(90, untouched.resolved(tokenSaved = false).timeoutMinutes)
        assertEquals("Mine", untouched.agent.name)
        assertEquals(
            listOf(AgentSection.Identity),
            agentChanges(theirs, untouched, tokenSaved = false, newToken = ""),
        )

        // Edited limit: my value is kept on top of the newer version.
        val edited = AgentDraft(base, timeout = "45").rebased(base, theirs)
        assertEquals("45", edited.timeout)
        assertEquals(45, edited.resolved(tokenSaved = false).timeoutMinutes)

        // The GitHub choice is kept as well.
        val github = AgentDraft(base, github = GithubMode.None.value).rebased(base, theirs).github
        assertEquals(GithubMode.None.value, github)
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
            ungrantable.unavailableGrants(holder.plugins).map { it.id },
        )
        assertEquals(listOf("onepassword"), ungrantable.agentGrantable().map { it.first.id })
        val issues = agentIssues(holder, resources.copy(catalog = ungrantable))
        assertEquals(3, issues.size)
        assertTrue(issues.all { it.section == AgentSection.Plugins })
        assertTrue(issues.any { "« Coffre » n’est plus disponible" in it.text })
        assertEquals("3 points à vérifier", issuesLabel(issues.size))

        // An instance it does not hold is not reported.
        assertTrue(ungrantable.unavailableGrants(listOf("prod")).isEmpty())
    }

    @Test
    fun aSavedProfileKeepsOnlyTheFailedTokenPendingFromTheNewRevision() {
        val draft =
            AgentDraft(agent)
                .copy(
                    agent = agent.copy(name = "Releases"),
                    github = GithubMode.Dedicated.value,
                )
        val saved = draft.resolved(tokenSaved = false).copy(revision = "r2")

        // The profile was saved, the token was not: only the token remains to save.
        val retry = draft.afterProfileSaved(saved)
        assertFalse(profileChanged(saved, retry.resolved(tokenSaved = false)))
        assertEquals(
            listOf(AgentSection.Github),
            agentChanges(saved, retry, tokenSaved = false, newToken = "ghp_x"),
        )
        assertEquals("ghp_x", retry.tokenChange(false, "ghp_x"))
        assertEquals(
            "r2",
            agentSaveBody(retry.resolved(false))["revision"]!!.jsonPrimitive.content,
        )

        // A pending removal of the saved token stays pending the same way.
        val removal =
            AgentDraft(agent)
                .withGithub(GithubMode.None, tokenSaved = true)
                .afterProfileSaved(agent.copy(revision = "r2"))
        assertEquals("", removal.tokenChange(tokenSaved = true, newToken = ""))
        assertEquals(
            listOf(AgentSection.Github),
            agentChanges(agent.copy(revision = "r2"), removal, tokenSaved = true, newToken = ""),
        )

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
        assertFalse(access["github"]!!.jsonPrimitive.content.toBoolean())
        assertFalse(access["installation"]!!.jsonPrimitive.content.toBoolean())
        assertEquals("yolo", access["sandbox"]!!.jsonPrimitive.content)
        assertEquals(GithubMode.None, AgentDraft(created).githubMode(tokenSaved = false))

        // A saved agent without access fields still decodes to the server defaults.
        val existing = wireJson.decodeFromString<Agent>("""{"id":"a","name":"Old"}""")
        assertNull(existing.access.projects)
        assertTrue(existing.access.github)
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
        assertTrue(agentChanges(omitted, AgentDraft(explicit), false, "").isEmpty())
        assertFalse(profileChanged(omitted, explicit))
    }
}
