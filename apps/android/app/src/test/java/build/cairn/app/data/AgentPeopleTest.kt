package build.cairn.app.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AgentPeopleTest {
    private val reviewer = "00000000-0000-4000-8000-000000000002"
    private val writer = "00000000-0000-4000-8000-000000000003"

    private fun agent(id: String) =
        Agent(
            id = id,
            name = id,
            revision = "r",
            createdAt = 0,
            exclusiveNodes = emptyList(),
            plugins = emptyList(),
            access =
                AccessPolicy(
                    mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                    projects = null,
                    skills = null,
                    mcps = null,
                ),
        )

    @Test
    fun theOwnerUsesEveryAgentAndMembersOnlyTheirs() {
        assertTrue(canUseAgent(null, reviewer))
        assertTrue(canUseAgent(listOf(reviewer), reviewer))
        assertFalse(canUseAgent(listOf(reviewer), writer))
        assertFalse(canUseAgent(emptyList(), null))
    }

    @Test
    fun aDelegatedTaskFollowsItsRequestingConversationWhileItExists() {
        val requesting = Chat(id = "requesting", agentId = reviewer)
        val delegated =
            Chat(id = "delegated", agentId = writer, requester = ChatRequester("requesting"))
        assertTrue(canDirectChat(listOf(reviewer), delegated, listOf(requesting, delegated)))
        assertFalse(canDirectChat(listOf(reviewer), delegated, listOf(delegated)))
        assertTrue(canDirectChat(listOf(writer), delegated, emptyList()))
        assertFalse(
            canDirectChat(emptyList(), Chat(id = "alone", agentId = writer), listOf(requesting))
        )
    }

    @Test
    fun newConversationsStartOnMainOnlyWhenItIsUsable() {
        val agents = listOf(agent(MAIN_AGENT_ID), agent(reviewer), agent(writer))
        assertEquals(MAIN_AGENT_ID, defaultAgent(null, agents))
        assertEquals(writer, defaultAgent(listOf(writer), agents))
        assertEquals("", defaultAgent(emptyList(), agents))
    }

    @Test
    fun everyMemberAppearsOnceAndFormerMembersStayRemovable() {
        val rows =
            agentAccessRows(
                listOf(
                    InstallationMember("a", "ada@example.test"),
                    InstallationMember("b", "bo@example.test"),
                ),
                listOf("b", "gone"),
            )
        assertEquals(
            listOf(
                AgentAccessRow("a", "ada@example.test", known = true, allowed = false),
                AgentAccessRow("b", "bo@example.test", known = true, allowed = true),
                AgentAccessRow("gone", "Ancien membre", known = false, allowed = true),
            ),
            rows,
        )
        assertEquals("Vous seul", agentAccessSummary(0))
        assertEquals("Vous et 1 membre", agentAccessSummary(1))
        assertEquals("Vous et 3 membres", agentAccessSummary(3))
    }

    @Test
    fun aResponseChangesOnlyItsOwnMember() {
        // Two quick grants: the first response, listing only its own member, arrives last.
        val afterSecond = withAccess(emptyList(), "b", listOf("a", "b"))
        assertEquals(listOf("b", "a"), withAccess(afterSecond, "a", listOf("a")))
        assertEquals(listOf("b"), withAccess(listOf("a", "b"), "a", listOf("b")))
        assertEquals(listOf("a"), withAccess(listOf("a"), "a", listOf("a")))
    }
}
