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
                        PluginConnection(
                            agentIds = listOf("08c10fb1-ed4e-5f3d-9592-dbfcefd1cdd3"),
                            settings = kotlinx.serialization.json.JsonObject(emptyMap()),
                            status = PluginStatus(),
                            usage = PluginUsage(),
                            enabled = true,
                        )
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
                listOf(
                    PluginConnection(
                        agentIds = listOf("08c10fb1-ed4e-5f3d-9592-dbfcefd1cdd3"),
                        settings = kotlinx.serialization.json.JsonObject(emptyMap()),
                        status = PluginStatus(),
                        usage = PluginUsage(),
                        enabled = true,
                    )
                ),
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
}
