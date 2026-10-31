package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.text.AnnotatedString
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

// The generated message inputs require a UUID conversation id.
private const val CHAT = "00000000-0000-4000-8000-0000000000c1"

abstract class PlanTurnCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun attachmentOnlyPlanRequestKeepsItsIndicator() {
        MockWebServer().use { server ->
            val chat = Chat(id = CHAT, title = "Plan joint", runId = "run", agentId = MAIN_AGENT_ID)
            val attachment =
                ChatAttachment(
                    id = "00000000-0000-4000-8000-0000000000a1",
                    name = "design.txt",
                    kind = "file",
                )
            val event =
                RunEvent(
                    id = 1,
                    runId = "run",
                    createdAt = 1789315189000,
                    type = "chat.user",
                    text = "",
                    payload =
                        buildJsonObject {
                            put("messageId", "attachment-plan")
                            put("text", "")
                            put("plan", true)
                            put("attachments", wireJson.encodeToJsonElement(listOf(attachment)))
                        },
                )
            val vm = mountPlanChat(server, chat, listOf(event), CopyOnWriteArrayList())
            try {
                compose.waitUntil(20000) {
                    compose
                        .onAllNodesWithText("Plan · ", substring = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("design.txt").assertExists()
                compose.onNodeWithText("Plan · ", substring = true).assertExists()
                capture("attachment-only-plan")
            } finally {
                vm.api.closeStreams()
            }
        }
    }

    @Test
    fun planRequestsShowTheirBadgeAndImplementingTheProposedPlanSendsAnOrdinaryMessage() =
        approvalJourney(false)

    @Test
    fun staleImplementationApprovalReportsTheConflictAndKeepsTheComposerEmpty() =
        approvalJourney(true)

    @Test
    fun completedProposalAfterSteeringUsesTheFinalSourceInItsHttpApproval() =
        approvalJourney(false, steeringBeforeCompletion = true)

    private fun approvalJourney(staleApproval: Boolean, steeringBeforeCompletion: Boolean = false) {
        MockWebServer().use { server ->
            val streamRequests = AtomicInteger()
            val approvalResponse = CountDownLatch(1)
            val calls = CopyOnWriteArrayList<String>()
            val chat =
                Chat(
                    id = CHAT,
                    title = "Composeur",
                    revision = "reviewed-revision",
                    agentId = MAIN_AGENT_ID,
                    agentName = "Agent principal",
                    runId = "run",
                    run =
                        Run(
                            id = "run",
                            taskId = CHAT,
                            status = "succeeded",
                            trigger = "chat",
                            snapshot =
                                Snapshot(
                                    task =
                                        build.cairn.contracts.RunTaskSnapshot(
                                            agentId = MAIN_AGENT_ID,
                                            id = "00000000-0000-4000-8000-000000000000",
                                            name = "Composeur",
                                            prompt = "",
                                        ),
                                    agent = newAgent().copy(provider = "claude"),
                                ),
                            chatExecution = ChatExecution(messageId = "plan-request", plan = true),
                        ),
                )
            fun proposalEvent(eventId: Long, kind: String) =
                RunEvent(
                    id = eventId,
                    runId = "run",
                    createdAt = 1789315189000 + eventId * 1000,
                    type = kind,
                    text = "",
                    payload =
                        buildJsonObject {
                            put(
                                "item",
                                buildJsonObject {
                                    put("id", "exit-plan-tool")
                                    put("type", "proposed_plan")
                                    put("text", "1. Lire le composeur\n2. Ajouter le bouton Plan")
                                    if (kind == "item.completed")
                                        put(
                                            "request_id",
                                            if (steeringBeforeCompletion) "clarification"
                                            else "plan-request",
                                        )
                                    if (kind == "item.completed") put("status", "completed")
                                },
                            )
                        },
                )
            val completedSource = if (steeringBeforeCompletion) 5L else 4L
            val events =
                listOfNotNull(
                    RunEvent(
                        id = 1,
                        createdAt = 1789315189000,
                        type = "chat.user",
                        text = "Planifie le nouveau composeur",
                        payload =
                            buildJsonObject {
                                put("text", "Planifie le nouveau composeur")
                                put("messageId", "plan-request")
                                put("plan", true)
                            },
                    ),
                    RunEvent(id = 2, createdAt = 1789315190000, type = "turn.started", text = ""),
                    proposalEvent(3, "item.started"),
                    if (steeringBeforeCompletion)
                        RunEvent(
                            id = 4,
                            runId = "run",
                            createdAt = 1789315192500,
                            type = "chat.user",
                            text = "Clarifie aussi les tests",
                            payload =
                                buildJsonObject {
                                    put("text", "Clarifie aussi les tests")
                                    put("messageId", "clarification")
                                    put("plan", true)
                                },
                        )
                    else null,
                    proposalEvent(completedSource, "item.completed"),
                    RunEvent(
                        id = completedSource + 1,
                        createdAt = 1789315195000,
                        type = "turn.completed",
                        text = "",
                    ),
                )
            val vm =
                mountPlanChat(
                    server,
                    chat,
                    events,
                    calls,
                    rejectApproval = staleApproval,
                    streamRequests = streamRequests,
                    approvalResponse = approvalResponse,
                )
            compose.waitUntil(20000) {
                compose.onAllNodesWithTag("proposed-plan").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Plan proposé").assertExists()
            compose
                .onAllNodesWithText("Plan · ", substring = true)
                .assertCountEquals(if (steeringBeforeCompletion) 2 else 1)
            capture("plan-turn")

            // An idle proposal can be explicitly implemented as an ordinary message.
            val initialStreams = streamRequests.get()
            try {
                compose.onNodeWithTag("implement-plan").assertIsEnabled().performClick()
                compose.waitUntil(20000) { calls.size == 1 }
                // Request arrival is not acceptance. Hold the HTTP response to make that
                // distinction deterministic: the composer remains busy until it arrives.
                assertTrue(vm.state.value.busy)
                compose.onNodeWithTag("plan-toggle").assertIsNotEnabled()
            } finally {
                approvalResponse.countDown()
            }
            compose.waitUntil(20000) {
                // Deliver the response continuation on Robolectric's main looper too;
                // polling only StateFlow leaves that continuation queued.
                compose.waitForIdle()
                !vm.state.value.busy
            }
            val implementing = wireJson.parseToJsonElement(calls[0]).jsonObject
            assertEquals(false, implementing["plan"]?.jsonPrimitive?.boolean)
            val approval = implementing["planApproval"]!!.jsonObject
            assertEquals("reviewed-revision", approval["revision"]!!.jsonPrimitive.content)
            assertEquals("run:$completedSource", approval["sourceId"]!!.jsonPrimitive.content)
            assertEquals(
                "Implémente le plan que tu as proposé.",
                implementing["text"]?.jsonPrimitive?.content,
            )
            compose.onNodeWithTag("plan-toggle").assertIsNotSelected()
            if (staleApproval) {
                compose.waitUntil(20000) { streamRequests.get() > initialStreams }
                compose.waitUntil(20000) {
                    vm.state.value.error?.contains("proposed plan changed") == true
                }
                compose
                    .onNode(hasSetTextAction())
                    .assert(
                        SemanticsMatcher.expectValue(
                            SemanticsProperties.EditableText,
                            AnnotatedString(""),
                        )
                    )
                assertEquals(1, calls.size)
                vm.api.closeStreams()
                return@use
            }

            // A later explicit question can still be queued as a protected plan turn.
            compose.onNodeWithTag("plan-toggle").assertIsEnabled().performClick()
            compose.onNodeWithTag("plan-toggle").assertIsSelected()
            compose.onNode(hasSetTextAction()).performTextInput("Et pour les tests ?")
            compose.onNodeWithTag("conversation-send").performClick()
            compose.waitUntil(20000) { calls.size == 2 }
            val question = wireJson.parseToJsonElement(calls[1]).jsonObject
            assertEquals(true, question["plan"]?.jsonPrimitive?.boolean)
            assertEquals("Et pour les tests ?", question["text"]?.jsonPrimitive?.content)
            vm.api.closeStreams()
        }
    }

    @Test
    fun savedPlanIntentCannotBecomeOrdinaryWithOpencode() {
        MockWebServer().use { server ->
            val calls = CopyOnWriteArrayList<String>()
            val initialSnapshot = CountDownLatch(1)
            val chat =
                Chat(
                    id = CHAT,
                    title = "Intention Plan",
                    agentId = MAIN_AGENT_ID,
                    agentName = "Agent principal",
                    runId = "run",
                    run =
                        Run(
                            id = "run",
                            taskId = CHAT,
                            status = "succeeded",
                            trigger = "chat",
                            snapshot =
                                Snapshot(
                                    task =
                                        build.cairn.contracts.RunTaskSnapshot(
                                            agentId = MAIN_AGENT_ID,
                                            id = "00000000-0000-4000-8000-000000000000",
                                            name = "Intention Plan",
                                            prompt = "",
                                        ),
                                    agent = newAgent().copy(provider = "opencode"),
                                ),
                            chatExecution = ChatExecution(messageId = "ordinary-before-plan"),
                        ),
                )
            val vm =
                mountPlanChat(
                    server,
                    chat,
                    emptyList(),
                    calls,
                    ChatDraft(text = "Examiner sans modifier", provider = "opencode", plan = true),
                    initialSnapshot,
                )
            try {
                compose.waitUntil(20_000) {
                    compose.onAllNodesWithTag("plan-toggle").fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNodeWithTag("plan-toggle").assertIsSelected().assertIsEnabled()
                compose.onNodeWithTag("plan-unavailable").assertIsDisplayed()
                compose.onNodeWithContentDescription("Envoyer").assertIsNotEnabled()
                assertTrue(calls.isEmpty())

                compose.onNodeWithTag("model-picker").performClick()
                compose.onNode(hasText("Codex") and isSelectable()).performClick()
                compose.onNodeWithText("Terminé").performClick()
                compose.waitUntil(20_000) {
                    compose
                        .onAllNodesWithTag("model-settings-sheet")
                        .fetchSemanticsNodes()
                        .isEmpty()
                }
                compose.onNodeWithTag("plan-toggle").assertIsSelected()
                // The composer appears before its initial chat snapshot. Even a compatible
                // provider cannot send until the live conversation has loaded.
                compose.onNodeWithTag("conversation-send").assertIsNotEnabled()
                initialSnapshot.countDown()
                compose.waitUntil(20_000) {
                    compose
                        .onAllNodes(hasTestTag("conversation-send") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithTag("conversation-send").assertIsEnabled().performClick()
                compose.waitUntil(20_000) { calls.isNotEmpty() || vm.state.value.error != null }
                assertNull(
                    "Submission failed: ${vm.state.value.error}; calls=$calls",
                    vm.state.value.error,
                )
                assertEquals(1, calls.size)
                compose.waitUntil(20_000) {
                    compose.onAllNodesWithTag("conversation-send").fetchSemanticsNodes()
                    !vm.state.value.busy
                }
                val planned = wireJson.parseToJsonElement(calls[0]).jsonObject
                assertEquals(true, planned["plan"]?.jsonPrimitive?.boolean)
                assertEquals("codex", planned["provider"]?.jsonPrimitive?.content)

                compose.onNode(hasSetTextAction()).performTextInput("Demande ordinaire explicite")
                compose.onNodeWithTag("model-picker").performClick()
                compose.onNode(hasText("OpenCode") and isSelectable()).performClick()
                compose.onNodeWithText("Terminé").performClick()
                compose.waitUntil(20_000) {
                    compose
                        .onAllNodesWithTag("model-settings-sheet")
                        .fetchSemanticsNodes()
                        .isEmpty()
                }
                compose.onNodeWithTag("plan-toggle").assertIsSelected()
                compose.onNodeWithContentDescription("Envoyer").assertIsNotEnabled()
                assertEquals(1, calls.size)
                compose.onNodeWithTag("plan-toggle").performClick()
                compose.onNodeWithTag("plan-toggle").assertIsNotSelected()
                compose.onNodeWithTag("conversation-send").assertIsEnabled().performClick()
                compose.waitUntil(20_000) { calls.size == 2 }
                val ordinary = wireJson.parseToJsonElement(calls[1]).jsonObject
                assertEquals(false, ordinary["plan"]?.jsonPrimitive?.boolean)
                assertEquals("opencode", ordinary["provider"]?.jsonPrimitive?.content)
            } finally {
                initialSnapshot.countDown()
                vm.api.closeStreams()
            }
        }
    }

    @Test
    fun queuedPlanRevisionCannotApproveThePrecedingProposal() {
        pendingRevisionCannotImplement("queued")
    }

    @Test
    fun sendingPlanRevisionCannotApproveThePrecedingProposal() {
        pendingRevisionCannotImplement("sending")
    }

    @Test
    fun unfinishedPlanTurnCannotApproveALaterProposal() {
        pendingRevisionCannotImplement(null, "running")
    }

    @Test
    fun completedReceiptReplayCannotReviveAnApproachInvalidatedByClarification() {
        MockWebServer().use { server ->
            val calls = CopyOnWriteArrayList<String>()
            val chat =
                Chat(
                    id = CHAT,
                    title = "Reprise du plan",
                    agentId = MAIN_AGENT_ID,
                    run = Run(id = "run", taskId = CHAT, status = "succeeded", trigger = "chat"),
                )
            fun proposal(id: Long) =
                RunEvent(
                    id = id,
                    runId = "run",
                    createdAt = id,
                    type = "item.completed",
                    text = "",
                    payload =
                        buildJsonObject {
                            put(
                                "item",
                                buildJsonObject {
                                    put("id", "approach-a")
                                    put("type", "proposed_plan")
                                    put("text", "Proposition avant clarification")
                                    put("request_id", "request-a")
                                },
                            )
                        },
                )
            val clarification =
                RunEvent(
                    id = 2,
                    runId = "run",
                    createdAt = 2,
                    type = "chat.user",
                    text = "Clarifie le plan",
                    payload =
                        buildJsonObject {
                            put("messageId", "request-b")
                            put("text", "Clarifie le plan")
                        },
                )
            val vm =
                mountPlanChat(server, chat, listOf(proposal(1), clarification, proposal(3)), calls)
            try {
                compose.waitUntil(20000) {
                    compose.onAllNodesWithTag("proposed-plan").fetchSemanticsNodes().isNotEmpty()
                }
                // Markdown renders through its native wrapper, not a Compose Text node.
                compose.onNodeWithTag("proposed-plan").assertExists()
                compose.onNodeWithText("Plan proposé").assertExists()
                compose.onAllNodesWithTag("implement-plan").assertCountEquals(0)
                assertTrue(calls.isEmpty())
            } finally {
                vm.api.closeStreams()
            }
        }
    }

    private fun pendingRevisionCannotImplement(status: String?, runStatus: String = "succeeded") {
        MockWebServer().use { server ->
            val calls = CopyOnWriteArrayList<String>()
            val chat =
                Chat(
                    id = CHAT,
                    title = "Révision du plan",
                    agentId = MAIN_AGENT_ID,
                    paused = status == "queued",
                    run = Run(id = "run", taskId = CHAT, status = runStatus, trigger = "chat"),
                    messages =
                        if (status == null) emptyList()
                        else
                            listOf(
                                ChatMessage(
                                    id = "revision",
                                    chatId = CHAT,
                                    text = "Révise la proposition",
                                    plan = true,
                                    status = status,
                                    createdAt = 1789315193000,
                                )
                            ),
                )
            val events =
                listOf(
                    RunEvent(
                        id = 1,
                        createdAt = 1789315191000,
                        type = "item.completed",
                        text = "",
                        payload =
                            buildJsonObject {
                                put(
                                    "item",
                                    buildJsonObject {
                                        put("id", "original-plan")
                                        put("type", "proposed_plan")
                                        put("text", "Proposition initiale")
                                    },
                                )
                            },
                    ),
                    RunEvent(id = 2, createdAt = 1789315192000, type = "turn.completed", text = ""),
                )
            val vm = mountPlanChat(server, chat, events, calls)
            try {
                compose.waitUntil(20000) {
                    compose.onAllNodesWithTag("implement-plan").fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNodeWithTag("implement-plan").assertIsNotEnabled()
                assertTrue(calls.isEmpty())
            } finally {
                vm.api.closeStreams()
            }
        }
    }

    private fun mountPlanChat(
        server: MockWebServer,
        chat: Chat,
        events: List<RunEvent>,
        calls: CopyOnWriteArrayList<String>,
        savedDraft: ChatDraft? = null,
        initialSnapshot: CountDownLatch? = null,
        rejectApproval: Boolean = false,
        streamRequests: AtomicInteger? = null,
        approvalResponse: CountDownLatch? = null,
    ): CairnViewModel {
        val cards =
            events
                .filter { it.type == "chat.user" }
                .map { event ->
                    val payload = event.payload
                    val attachments =
                        payload
                            ?.get("attachments")
                            ?.let {
                                wireJson.decodeFromJsonElement<List<ChatAttachment>>(it)
                            }
                            .orEmpty()
                    build.cairn.contracts.TimelineChange.A(
                        TimelineCard(
                            id = "user:${event.id}",
                            runId = "run",
                            position = event.id,
                            revision = event.id,
                            source = event.id,
                            turn = 0,
                            attempt = 0,
                            createdAt = event.createdAt,
                            updatedAt = event.createdAt,
                            content =
                                build.cairn.contracts.TimelineContent.A(
                                    text = payload?.string("text") ?: event.text,
                                    messageId = payload?.string("messageId"),
                                    plan = payload?.get("plan")?.jsonPrimitive?.booleanOrNull,
                                    attachments = attachments,
                                    sentAt = event.createdAt,
                                ),
                        )
                    )
                }
        val markers = events.map { event ->
            if (event.type == "chat.user") event.copy(text = "", payload = null, timeline = true)
            else event
        }
        val batch =
            wireJson.encodeToString(
                LiveBatch(markers, LiveState(chat = chat), true, false, entries = cards)
            )
        server.dispatcher =
            object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val path = request.path!!.substringBefore('?')
                    if (path == "/api/installations/fixture/api/chats/$CHAT/stream") {
                        streamRequests?.incrementAndGet()
                        check(initialSnapshot?.await(20, TimeUnit.SECONDS) != false)
                        val initial =
                            "event: batch\nid: ${events.maxOfOrNull { it.id } ?: 0L}\ndata: $batch\n\n"
                        return MockResponse()
                            .setHeader("Content-Type", "text/event-stream")
                            .setBody(initial + ": keepalive\n".repeat(100_000))
                            .throttleBody(
                                initial.toByteArray().size.toLong(),
                                250,
                                TimeUnit.MILLISECONDS,
                            )
                    }
                    if (path == "/api/installations/fixture/api/chats/$CHAT/messages") {
                        val sent = request.body.readUtf8()
                        calls += sent
                        check(approvalResponse?.await(20, TimeUnit.SECONDS) != false)
                        if (rejectApproval)
                            return MockResponse()
                                .setResponseCode(409)
                                .setHeader("Content-Type", "application/json")
                                .setBody(
                                    """{"error":"The proposed plan changed. Refresh the conversation and review it before implementing."}"""
                                )
                        // The accepted message echoes its id, like the installation.
                        val id = wireJson.parseToJsonElement(sent).jsonObject["id"]
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(buildJsonObject { put("id", id!!) }.toString())
                    }
                    val body =
                        when (path) {
                            "/api/account/session" -> beaconAccountFixture()
                            "/api/installations" -> beaconInstallationsFixture()
                            "/api/installations/fixture/api/agents" ->
                                wireJson.encodeToString(
                                    build.cairn.contracts.ListAgentsOutput(
                                        listOf(
                                            newAgent()
                                                .copy(
                                                    id = MAIN_AGENT_ID,
                                                    name = "Agent principal",
                                                    provider =
                                                        chat.run?.snapshot?.agent?.provider
                                                            ?: "claude",
                                                )
                                        )
                                    )
                                )
                            "/api/installations/fixture/api/chats/$CHAT" ->
                                wireJson.encodeToString(chat)
                            "/api/installations/fixture/api/projects" -> """{"projects":[]}"""
                            "/api/installations/fixture/api/tasks" -> """{"tasks":[]}"""
                            "/api/installations/fixture/api/mcps" -> """{"mcps":[]}"""
                            "/api/installations/fixture/api/overview" -> "{}"
                            else -> "[]"
                        }
                    return MockResponse()
                        .setHeader("Content-Type", "application/json")
                        .setBody(body)
                }
            }
        server.start()
        val vm =
            CairnViewModel(
                ApplicationProvider.getApplicationContext<Application>(),
                MemoryVault().apply {
                    write(
                        server.url("/").toString(),
                        "cairn_session=fixture; Path=/; Max-Age=3600",
                    )
                },
                beaconOrigin = "",
            )
        val connected = mutableStateOf(false)
        compose.setContent {
            val state by vm.state.collectAsStateWithLifecycle()
            LaunchedEffect(Unit) {
                vm.state.first { it.ready && !it.busy }
                vm.connect(server.url("/").toString())
                if (savedDraft != null) vm.chatDrafts[CHAT] = savedDraft
                connected.value = true
            }
            CairnTheme(preference = "system") {
                if (state.session.authenticated && connected.value)
                    Surface(
                        Modifier.fillMaxSize(),
                        color = MaterialTheme.colorScheme.background,
                    ) {
                        Box(Modifier.safeDrawingPadding()) {
                            ChatScreen(
                                vm,
                                state,
                                CHAT,
                                openChat = {},
                                openRun = {},
                                back = {},
                            )
                        }
                    }
            }
        }
        return vm
    }

    protected open fun capture(name: String) {}
}
