package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * A member without access to a conversation's agent follows it without directing it (ADR-0075), and
 * sees the conversation again as theirs once a refused action reveals the owner's change.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class MemberConversationTest {
    @get:Rule val compose = createComposeRule()

    private val chat =
        Chat(
            "shared",
            "Préparer la version 0.53",
            agentId = MAIN_AGENT_ID,
            agentName = "Agent principal",
            status = "succeeded",
            paused = true,
            provider = "codex",
        )

    private fun stream(state: LiveState): MockResponse {
        val frame =
            "event: batch\nid: 1\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), state, true, false))}\n\n"
        return MockResponse()
            .setHeader("Content-Type", "text/event-stream")
            .setBody(frame + ": keepalive\n\n".repeat(100000))
            .throttleBody(frame.toByteArray().size.toLong(), 1, TimeUnit.SECONDS)
    }

    @Test
    fun `a member without access follows the conversation without directing it`() {
        val usable = AtomicReference("[]")
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (path.endsWith("/chats/shared/stream"))
                            return stream(LiveState(chat = chat))
                        if (path.endsWith("/chats/stream"))
                            return stream(LiveState(chats = listOf(chat)))
                        val response =
                            when {
                                path == "/api/account/session" ->
                                    beaconAccountFixture().replace("\"owner\"", "\"member\"")
                                path == "/api/installations" ->
                                    beaconInstallationsFixture().replace("\"owner\"", "\"member\"")
                                path.endsWith("/usable-agents") ->
                                    """{"agentIds":${usable.get()}}"""
                                path.endsWith("/api/projects") -> """{"projects":[]}"""
                                path.endsWith("/api/agents") -> """{"agents":[]}"""
                                path.endsWith("/api/tasks") -> """{"tasks":[]}"""
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
                                else -> "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(response)
                    }
                }
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MemoryVault(),
                    beaconOrigin = "",
                )
            compose.setContent {
                val state by vm.state.collectAsState()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (state.installation != null)
                        ChatScreen(vm, state, "shared", openChat = {}, openRun = {})
                }
            }
            fun waitFor(condition: () -> Boolean) =
                compose.waitUntil(20000) {
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    condition()
                }
            try {
                waitFor { vm.state.value.usableAgents == emptyList<String>() }
                waitFor {
                    compose
                        .onAllNodesWithTag("conversation-read-only")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose
                    .onNodeWithText("Vous pouvez suivre cette conversation", substring = true)
                    .assertExists()
                compose
                    .onNode(
                        hasSetTextAction() and hasAnyAncestor(hasTestTag("conversation-composer"))
                    )
                    .assertIsNotEnabled()
                compose.onNodeWithTag("conversation-send").assertIsNotEnabled()

                // Resuming needs access; pausing and reading do not.
                compose.onNodeWithContentDescription("Options de la conversation").performClick()
                compose.onNodeWithText("Détails de la conversation").assertExists()
                compose.onAllNodesWithText("Reprendre").assertCountEquals(0)
                compose.onNodeWithText("Suivre les réponses").performClick()

                // The owner gives access: a refused action shows the current agents at once.
                usable.set("[\"$MAIN_AGENT_ID\"]")
                compose.runOnIdle { vm.report(ApiException(403, "Refused")) }
                waitFor { vm.state.value.usableAgents == listOf(MAIN_AGENT_ID) }
                waitFor {
                    compose
                        .onAllNodesWithTag("conversation-read-only")
                        .fetchSemanticsNodes()
                        .isEmpty()
                }
                assertEquals(listOf(MAIN_AGENT_ID), vm.state.value.usableAgents)
            } finally {
                vm.api.closeStreams()
            }
        }
    }
}
