package build.cairn.app.data

import kotlinx.serialization.json.JsonNull
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
        assertEquals(emptyList<String>(), old.access.exclusiveNodes)
        assertEquals(emptyList<String>(), old.access.plugins)
        assertEquals("", old.revision)

        val current =
            wireJson.decodeFromString<Agent>(
                """{"id":"a","name":"New","revision":"r9","access":{"exclusiveNodes":["gpu"],"plugins":["prod"]}}"""
            )
        assertEquals(listOf("gpu"), current.access.exclusiveNodes)
        assertEquals(listOf("prod"), current.access.plugins)
        assertEquals("r9", current.revision)
    }

    @Test
    fun saveBodySendsExplicitAllSelectionsRevisionAndReapprovals() {
        val body =
            agentSaveBody(
                agent.copy(
                    access =
                        agent.access.copy(nodes = null, skills = null, plugins = listOf("prod"))
                ),
                listOf("bot"),
            )
        val access = body["access"]!!.jsonObject
        assertEquals(JsonNull, access["nodes"])
        assertEquals(JsonNull, access["skills"])
        assertEquals(JsonNull, access["mcps"])
        assertEquals(2, access["projects"]!!.jsonArray.size)
        assertEquals("prod", access["plugins"]!!.jsonArray.single().jsonPrimitive.content)
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
        val granted = agent.access.withExclusive("gpu", true)
        assertEquals(listOf(gpu), granted.effectiveExclusive(nodes))
        assertEquals("2 sur 3 · exclusivité sur 1", nodesSummary(granted, nodes))

        val stopped = granted.withNodeRun("gpu", false)
        assertEquals(listOf(LOCAL_NODE_ID), stopped.nodes)
        assertTrue(stopped.exclusiveNodes.isEmpty())
        assertEquals("Runner actuel seulement", nodesSummary(stopped, nodes))

        // Legacy data: an exclusive grant where the agent cannot run is inactive and reported.
        val legacy = stopped.copy(exclusiveNodes = listOf("gpu"))
        assertTrue(legacy.effectiveExclusive(nodes).isEmpty())
        assertEquals(
            AgentSection.Nodes,
            agentIssues(agent.copy(access = legacy), resources).single().section,
        )

        // "All nodes" counts every node as executable; turning it off keeps today's machines.
        val all = legacy.withAllNodes(true, nodes)
        assertEquals(listOf(gpu), all.effectiveExclusive(nodes))
        assertEquals(
            listOf(LOCAL_NODE_ID, "gpu", "mac"),
            all.withAllNodes(false, nodes).nodes,
        )
    }

    @Test
    fun summariesUseTheSharedWording() {
        assertEquals("Tous, y compris futurs", selectionSummary(null, 12))
        assertEquals("Toutes, y compris futures", selectionSummary(null, 12, feminine = true))
        assertEquals("3 sur 12", selectionSummary(listOf("a", "b", "c"), 12))
        assertEquals("Aucun", selectionSummary(emptyList(), 12))
        assertEquals("", pluginSummary(agent.access, catalog))
        assertEquals(
            "1Password : 2 comptes",
            pluginSummary(agent.access.copy(plugins = listOf("prod", "staging")), catalog),
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
                        access =
                            agent.access.copy(
                                plugins = listOf("prod"),
                                exclusiveNodes = listOf("gpu"),
                            ),
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
        assertEquals("", draft.copy(github = GithubMode.None.value).tokenChange(true, ""))
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
                access =
                    agent.access.copy(
                        projects = listOf("p1"),
                        exclusiveNodes = listOf("gpu"),
                        plugins = listOf("prod"),
                        installation = true,
                    )
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
                edited.copy(access = agent.access.copy(projects = listOf("p1"))),
                resources,
                AgentPreview(),
            )
        assertEquals(listOf(ReviewKind.Reduction), reduced.map { it.kind })
        assertFalse(needsReview(reduced, AgentPreview()))
    }

    @Test
    fun restrictingProjectsTurnsSharedGithubOffAndSaysSo() {
        val shared = agent.copy(access = agent.access.copy(projects = null, github = true))
        val restricted = shared.access.withAllProjects(false, projects)
        assertEquals(listOf("p1", "p2"), restricted.projects)
        assertFalse(restricted.github)
        val lines = agentReview(shared, shared.copy(access = restricted), resources, null)
        assertTrue(lines.any { it.text.startsWith("Connexion GitHub partagée désactivée") })
    }

    @Test
    fun issuesFlagDisabledAccountsAndMissingSkills() {
        val withGrants =
            agent.copy(
                access =
                    agent.access.copy(plugins = listOf("staging"), skills = listOf("global/gone"))
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
        val mine = agent.copy(name = "Mine", access = agent.access.copy(plugins = listOf("prod")))
        val theirs =
            agent.copy(
                description = "Theirs",
                revision = "r2",
                access = agent.access.copy(sandbox = "read-only"),
            )
        val rebased = rebaseAgent(agent, mine, theirs)
        assertEquals("Mine", rebased.name)
        assertEquals("Theirs", rebased.description)
        assertEquals("read-only", rebased.access.sandbox)
        assertEquals(listOf("prod"), rebased.access.plugins)
        assertEquals("r2", rebased.revision)
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
                .copy(github = GithubMode.None.value)
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
        assertEquals(0, access["exclusiveNodes"]!!.jsonArray.size)
        assertEquals(0, access["plugins"]!!.jsonArray.size)
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
                """{"id":"agent","name":"Release","revision":"r1","access":{"nodes":["$LOCAL_NODE_ID"],"exclusiveNodes":[],"plugins":[]}}"""
            )
        assertEquals(explicit, omitted)
        assertTrue(agentChanges(omitted, AgentDraft(explicit), false, "").isEmpty())
        assertFalse(profileChanged(omitted, explicit))
    }
}
