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
        assertFalse(draft.withGithub(GithubMode.Dedicated).switchedToSavedToken(tokenSaved = true))

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
    fun reviewNamesTheRemovedTokenNewToolChoicesAndOtherwiseInterruptedWork() {
        val dedicated = AgentDraft(agent).withGithub(GithubMode.None)
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
            if ("add" in body) throw ApiException(400, "Unknown field add")
        }
        assertEquals(2, sent.size)
        assertFalse("add" in sent.last())
        assertEquals(
            setOf("other", "agent"),
            sent.last()["agentIds"]!!.jsonArray.map { it.jsonPrimitive.content }.toSet(),
        )

        // Other failures are not retried.
        sent.clear()
        val failed = runCatching {
            grantNodeAgents(listOf("agent"), setOf("other")) { body ->
                sent += body
                throw ApiException(500, "Erreur")
            }
        }
        assertTrue(failed.isFailure)
        assertEquals(1, sent.size)
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
