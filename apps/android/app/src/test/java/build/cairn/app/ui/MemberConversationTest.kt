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

    @org.junit.Before
    fun initializeWork() {
        androidx.work.testing.WorkManagerTestInitHelper.initializeTestWorkManager(
            ApplicationProvider.getApplicationContext<Application>(),
            androidx.work.Configuration.Builder()
                .setExecutor(androidx.work.testing.SynchronousExecutor())
                .build(),
        )
    }

    @org.junit.After
    fun closeWork() {
        androidx.work.testing.WorkManagerTestInitHelper.closeWorkDatabase()
    }

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

    /** Opens [chat] as a member whose usable agents the server reads from [usable]. */
    private fun asMember(
        chat: Chat,
        usable: AtomicReference<String>,
        check: (CairnViewModel, (() -> Boolean) -> Unit) -> Unit,
    ) {
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (path.endsWith("/chats/${chat.id}/stream"))
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
                        ChatScreen(vm, state, chat.id, openChat = {}, openRun = {})
                }
            }
            fun waitFor(condition: () -> Boolean) =
                compose.waitUntil(20000) {
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    condition()
                }
            try {
                check(vm, ::waitFor)
            } finally {
                vm.api.closeStreams()
            }
        }
    }

    @Test
    fun `a member without access follows the conversation without directing it`() {
        val usable = AtomicReference("[]")
        asMember(chat, usable) { vm, waitFor ->
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
            // The composer offers no editing, only the reason.
            compose
                .onAllNodes(
                    hasSetTextAction() and hasAnyAncestor(hasTestTag("conversation-composer"))
                )
                .assertCountEquals(0)
            compose.onNodeWithText("Réservé aux personnes ayant accès à cet agent").assertExists()
            compose.onNodeWithTag("conversation-send").assertIsNotEnabled()
            compose.onNodeWithContentDescription("Joindre").assertIsNotEnabled()

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
                compose.onAllNodesWithTag("conversation-read-only").fetchSemanticsNodes().isEmpty()
            }
            assertEquals(listOf(MAIN_AGENT_ID), vm.state.value.usableAgents)
        }
    }

    @Test
    fun `only people with access to its agent are offered to restore a deleted conversation`() {
        val trashed =
            chat.copy(
                id = "trashed",
                lifecycle = "trash",
                purgeAt = System.currentTimeMillis() + 86_400_000,
            )
        asMember(trashed, AtomicReference("[]")) { _, waitFor ->
            waitFor {
                compose
                    .onAllNodesWithText("Cette conversation est dans la corbeille.")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose
                .onNodeWithText(
                    "Seules les personnes ayant accès à Agent principal",
                    substring = true,
                )
                .assertExists()
            compose.onAllNodesWithText("Restaurer la conversation").assertCountEquals(0)
        }
    }
}
