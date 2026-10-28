package build.cairn.app.data

import org.junit.Assert.assertEquals
import org.junit.Test

/** The owner's warning about what a bot's people can do; nothing is refused. */
class PluginDelegationTest {
    private fun agent(access: AccessPolicy) =
        Agent(
            exclusiveNodes = emptyList(),
            plugins = emptyList(),
            revision = "",
            createdAt = 0,
            id = "08c10fb1-ed4e-5f3d-9592-dbfcefd1cdd3",
            name = "Helper",
            access = access,
        )

    private val limited =
        AccessPolicy(
            mcpTools = build.cairn.contracts.McpTools(emptyMap()),
            skills = null,
            projects = emptyList(),
            mcps = emptyList(),
            sandbox = "read-only",
        )

    @Test
    fun `native instance explains required owner delegation`() {
        val warning = agent(limited.copy(backend = "native")).externalBotExecutionWarning()
        org.junit.Assert.assertTrue(
            warning!!.contains("exige une délégation native actuelle du propriétaire")
        )
        org.junit.Assert.assertNull(
            agent(limited.copy(backend = "firecracker")).externalBotExecutionWarning()
        )
    }

    @Test
    fun `resource route retains native execution restriction`() {
        val warning =
            agent(limited.copy(backend = "native"))
                .externalBotExecutionWarning(resourceRoute = true)
        org.junit.Assert.assertTrue(warning!!.contains("cette route n'a pas de délégation native"))
        org.junit.Assert.assertNull(
            agent(limited.copy(backend = "firecracker"))
                .externalBotExecutionWarning(resourceRoute = true)
        )
    }

    @Test
    fun `broad agents get a warning for listed people`() {
        assertEquals(
            emptyList<String>(),
            agent(limited).delegationRisks(emptyList(), everyone = false),
        )
        assertEquals(
            emptyList<String>(),
            agent(limited.copy(sandbox = "yolo", mcps = null)).delegationRisks(emptyList(), false),
        )
        assertEquals(
            listOf("gérer toute votre installation", "ouvrir tous les projets"),
            agent(limited.copy(installation = true, projects = null))
                .delegationRisks(emptyList(), everyone = false),
        )
        assertEquals(
            listOf("ouvrir 2 projets"),
            agent(limited.copy(projects = listOf("a", "b"))).delegationRisks(emptyList(), false),
        )
    }

    @Test
    fun `anyone in a channel is also warned about writing, MCP and plugins`() {
        assertEquals(
            emptyList<String>(),
            agent(limited).delegationRisks(emptyList(), everyone = true),
        )
        assertEquals(
            listOf(
                "modifier des fichiers et lancer des commandes",
                "utiliser ses connexions MCP",
                "utiliser ses plugins, comme GitHub ou 1Password",
            ),
            agent(limited.copy(sandbox = "yolo", mcps = null))
                .delegationRisks(
                    listOf(
                        PluginConnection(agentIds = listOf("08c10fb1-ed4e-5f3d-9592-dbfcefd1cdd3"))
                    ),
                    everyone = true,
                ),
        )
    }

    @Test
    fun `warns about grants to the edited instance and unsaved instance`() {
        val helper = agent(limited.copy(projects = listOf("project")))
        val warning = listOf("utiliser ses plugins, comme GitHub ou 1Password")
        assertEquals(
            warning,
            helper.delegationRisks(
                listOf(PluginConnection(agentIds = listOf("08c10fb1-ed4e-5f3d-9592-dbfcefd1cdd3"))),
                true,
            ),
        )
        assertEquals(
            warning,
            helper.delegationRisks(
                emptyList(),
                true,
                listOf("08c10fb1-ed4e-5f3d-9592-dbfcefd1cdd3"),
            ),
        )
        assertEquals(
            emptyList<String>(),
            helper.delegationRisks(emptyList(), true, listOf("other")),
        )
    }

    @Test
    fun `risks read as a sentence`() {
        assertEquals("", listed(emptyList()))
        assertEquals("a", listed(listOf("a")))
        assertEquals("a, b et c", listed(listOf("a", "b", "c")))
    }

    @Test
    fun `listed and public people get a warning about delegated work`() {
        for (everyone in listOf(false, true)) {
            assertEquals(
                listOf("déléguer du travail à d'autres agents de code et modèles"),
                agent(limited.copy(delegation = true)).delegationRisks(emptyList(), everyone),
            )
        }
    }

    @Test
    fun `resource permission metadata warns without an instance grant`() {
        val helper = agent(limited)
        assertEquals(
            listOf("utiliser ses plugins, comme GitHub ou 1Password"),
            helper.delegationRisks(emptyList(), everyone = true, pluginAccess = true),
        )
        assertEquals(emptyList<String>(), helper.delegationRisks(emptyList(), true))
        assertEquals(
            emptyList<String>(),
            helper.delegationRisks(emptyList(), false, pluginAccess = true),
        )
        val metadata =
            wireJson.decodeFromString<RouteEligibility>(
                """{"agentId":"helper","eligible":true,"pluginAccess":true}"""
            )
        assertEquals(true, metadata.pluginAccess)
    }
}
