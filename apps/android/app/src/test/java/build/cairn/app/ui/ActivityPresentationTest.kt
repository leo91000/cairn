package build.cairn.app.ui

import build.cairn.app.data.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ActivityPresentationTest {
    private fun event(type: String, text: String) = RunEvent(1, 1000, type, text)

    private fun tool(command: String, code: Int = 0) =
        RunEvent(
            1,
            1000,
            "item.completed",
            "",
            mapOf(
                "item" to
                    buildJsonObject {
                        put("type", "command_execution")
                        put("command", command)
                        put("exit_code", code)
                    }
            ),
        )

    @Test
    fun `background waits are labelled in the activity log`() {
        fun waiting(vararg descriptions: String) =
            RunEvent(
                1,
                1000,
                "turn.waiting",
                "",
                mapOf(
                    "type" to JsonPrimitive("turn.waiting"),
                    "tasks" to
                        buildJsonArray {
                            descriptions.forEach { addJsonObject { put("description", it) } }
                        },
                ),
            )
        val waiting = presentActivity(waiting("Build", "Watch CI"))
        assertEquals(ActivityKind.NOTICE, waiting.kind)
        assertEquals("En attente de tâches en arrière-plan", waiting.title)
        assertEquals("Build · Watch CI", waiting.subtitle)
        assertEquals("", waiting.output)
        val finished = presentActivity(waiting())
        assertEquals("Tâches en arrière-plan terminées", finished.title)
        assertEquals("", finished.subtitle)
        assertEquals("", finished.output)
    }

    @Test
    fun `session events from screenshot have human summaries and no JSON body`() {
        val done = presentActivity(event("turn.completed", """{"type":"turn.completed"}"""))
        assertEquals("Travail terminé", done.title)
        assertEquals("", done.output)
        assertEquals("Exécution réussie", presentActivity(event("status", "succeeded")).title)
        val account = presentActivity(event("status", "Using Codex account: Compte principal"))
        assertEquals("Compte Codex sélectionné", account.title)
        assertEquals("Compte principal", account.subtitle)
        val claude = presentActivity(event("status", "Using Claude Code account: Studio"))
        assertEquals("Compte Claude Code sélectionné", claude.title)
        assertEquals("Studio", claude.subtitle)
    }

    @Test
    fun `quoted file reads are identified but compound shells stay commands`() {
        assertEquals(listOf("docs/my file.md"), commandView("cat 'docs/my file.md'").paths)
        assertEquals(ActivityKind.READ, commandView("bash -lc 'sed -n 1,40p README.md'").kind)
        assertEquals(ActivityKind.COMMAND, commandView("cat README.md && rm old.txt").kind)
        assertEquals(ActivityKind.COMMAND, commandView("cat \$(pwd)/README.md").kind)
        assertEquals(ActivityKind.BROWSE, commandView("rg --files").kind)
    }

    @Test
    fun `normal search outcomes are separate from failing compound commands`() {
        assertFalse(presentActivity(tool("rg needle .", 1)).failed)
        assertEquals("Aucune correspondance", presentActivity(tool("rg needle .", 1)).state)
        assertTrue(presentActivity(tool("rg needle . && build", 1)).failed)
        assertTrue(presentActivity(tool("cargo test", 1)).failed)
    }

    @Test
    fun `MCP result error is reflected on the card`() {
        val item = buildJsonObject {
            put("type", "mcp_tool_call")
            put("tool", "github__checks")
            put("result", buildJsonObject { put("isError", true) })
        }
        val p = presentActivity(RunEvent(1, 1000, "item.completed", "", mapOf("item" to item)))
        assertTrue(p.failed)
        assertEquals(ActivityKind.TOOL, p.kind)
    }

    @Test
    fun `JSON embedded in text or a fence becomes a structured result`() {
        val parts =
            resultParts(
                "Results:\n{\"jobs\":[{\"name\":\"build\",\"conclusion\":\"success\"}]}\nDone"
            )
        assertEquals(1, parts.filterIsInstance<ResultPart.Data>().size)
        assertTrue(resultParts("```json\n[1,2]\n```").single() is ResultPart.Data)
        assertTrue(resultParts("[1](https://example.com)").single() is ResultPart.Text)
        assertTrue(resultParts("`{\"a\":1}`").single() is ResultPart.Text)
    }

    @Test
    fun `truncated JSON never presents a complete nested fragment`() {
        assertTrue(
            resultParts("{\"outer\":{\"ok\":true},\"tail\":").single() is ResultPart.Incomplete
        )
        assertTrue(resultParts("```json\n{\"outer\":").single() is ResultPart.Incomplete)
    }

    @Test
    fun `legacy saved tools remain tools and get a structured result`() {
        val rows =
            timelineEntries(
                listOf(
                    event("item.started", "Running tool"),
                    event("item.completed", "{\"jobs\":[]}").copy(id = 2),
                )
            )
        assertFalse(rows.single().message)
        val p = presentActivity(rows.single().events.single())
        assertEquals(ActivityKind.OUTPUT, p.kind)
        assertTrue(resultParts(p.output).single() is ResultPart.Data)
    }
}
