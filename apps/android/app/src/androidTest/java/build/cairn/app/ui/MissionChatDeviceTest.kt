package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import build.cairn.app.data.*
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import okhttp3.mockwebserver.*
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class MissionChatDeviceTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun waitingMissionShowsItsReasonAndCanBeCancelled() {
        val run =
            Run(
                "waiting",
                taskId = "mission",
                status = "waiting",
                trigger = "manual",
                wakeup =
                    ChatWakeup(
                        "wake",
                        reason = "Attendre la CI de la mission",
                        seconds = 420,
                        createdAt = System.currentTimeMillis(),
                        wakeAt = System.currentTimeMillis() + 420_000,
                    ),
            )
        val child =
            RunEvent(
                1,
                System.currentTimeMillis(),
                "item.started",
                "",
                mapOf(
                    "item" to
                        buildJsonObject {
                            put("id", "child")
                            put("type", "subagent")
                            put("name", "CI observer")
                            put("status", "in_progress")
                        }
                ),
            )
        val cancellations = java.util.concurrent.atomic.AtomicInteger()
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val body =
                            when {
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path.endsWith("/runs/waiting/cancel") -> {
                                    assertEquals("POST", request.method)
                                    cancellations.incrementAndGet()
                                    "{\"cancelled\":true}"
                                }
                                path.endsWith("/runs/waiting/stream") -> {
                                    val frame =
                                        "event: batch\nid: 1\ndata: ${wireJson.encodeToString(LiveBatch(listOf(child), LiveState(run = run), true, false))}\n\n"
                                    return MockResponse()
                                        .setHeader("Content-Type", "text/event-stream")
                                        .setBody(frame + ": keepalive\n\n".repeat(100000))
                                        .throttleBody(
                                            frame.toByteArray().size.toLong(),
                                            1,
                                            TimeUnit.SECONDS,
                                        )
                                }
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
                                else ->
                                    if (
                                        request.requestUrl
                                            ?.encodedPath
                                            ?.endsWith("/api/projects") == true
                                    )
                                        """{"projects":[]}"""
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/agents") ==
                                            true
                                    )
                                        """{"agents":[]}"""
                                    else "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            val origin = server.url("/").toString()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MissionChatVault(),
                    beaconOrigin = "",
                )
            compose.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(origin)
                }
                val state = vm.state.collectAsState().value
                CairnTheme("light") {
                    if (state.installation != null) RunScreen(vm, state, run.id, openRun = {})
                }
            }
            try {
                compose.waitUntil(15000) {
                    compose
                        .onAllNodesWithText("Attente programmée", substring = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose
                    .onAllNodesWithText("Attendre la CI de la mission", substring = true)
                    .onFirst()
                    .assertExists()
                // Stopped teams are collapsed; their summary still exposes the stopped state.
                compose.onNodeWithText("1 arrêté").assertExists()
                compose.onNodeWithText("Mettre en pause").assertDoesNotExist()
                compose.onNodeWithContentDescription("Arrêter").performClick()
                compose.onNodeWithText("Confirmer").performClick()
                compose.waitUntil(10000) { cancellations.get() == 1 }
            } finally {
                vm.api.closeStreams()
            }
        }
    }

    @Test
    fun completedMissionOpensTheConvertedChat() {
        val run =
            Run(
                "done",
                taskId = "mission",
                status = "succeeded",
                trigger = "manual",
                sessionId = "saved",
            )
        val chat = Chat("follow-up", runId = run.id)
        var opened = ""
        val conversions = java.util.concurrent.atomic.AtomicInteger()
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val body =
                            when {
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path.endsWith("/runs/done/chat") -> {
                                    assertEquals("POST", request.method)
                                    conversions.incrementAndGet()
                                    wireJson.encodeToString(chat)
                                }
                                path.endsWith("/runs/done/stream") -> {
                                    val frame =
                                        "event: batch\nid: 1\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), LiveState(run = run), true, false))}\n\n"
                                    return MockResponse()
                                        .setHeader("Content-Type", "text/event-stream")
                                        .setBody(frame + ": keepalive\n\n".repeat(100000))
                                        .throttleBody(
                                            frame.toByteArray().size.toLong(),
                                            1,
                                            TimeUnit.SECONDS,
                                        )
                                }
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
                                else ->
                                    if (
                                        request.requestUrl
                                            ?.encodedPath
                                            ?.endsWith("/api/projects") == true
                                    )
                                        """{"projects":[]}"""
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/agents") ==
                                            true
                                    )
                                        """{"agents":[]}"""
                                    else "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            val origin = server.url("/").toString()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MissionChatVault(),
                    beaconOrigin = "",
                )
            compose.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(origin)
                }
                val state = vm.state.collectAsState().value
                CairnTheme("light") {
                    if (state.installation != null)
                        RunScreen(vm, state, run.id, openChat = { opened = it }, openRun = {})
                }
            }
            try {
                compose.waitUntil(15000) {
                    compose
                        .onAllNodesWithContentDescription("Options de l’exécution")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithContentDescription("Options de l’exécution").performClick()
                compose.onNodeWithText("Continuer en chat").performClick()
                compose.waitUntil(10000) { opened == chat.id }
                assertEquals(1, conversions.get())
            } finally {
                vm.api.closeStreams()
            }
        }
    }

    @Test
    fun failedMissionResumesTheExistingRun() {
        val run =
            Run(
                "123e4567-e89b-42d3-a456-426614174000",
                taskId = "mission",
                status = "failed",
                resumeAvailable = true,
                trigger = "manual",
                sessionId = "saved",
            )
        val resumes = java.util.concurrent.atomic.AtomicInteger()
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val body =
                            when {
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path.endsWith("/runs/${run.id}/resume") -> {
                                    assertEquals("POST", request.method)
                                    resumes.incrementAndGet()
                                    """{"runId":"${run.id}"}"""
                                }
                                path.endsWith("/runs/${run.id}/stream") -> {
                                    val frame =
                                        "event: batch\nid: 1\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), LiveState(run = run), true, false))}\n\n"
                                    return MockResponse()
                                        .setHeader("Content-Type", "text/event-stream")
                                        .setBody(frame + ": keepalive\n\n".repeat(100000))
                                        .throttleBody(
                                            frame.toByteArray().size.toLong(),
                                            1,
                                            TimeUnit.SECONDS,
                                        )
                                }
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
                                else ->
                                    if (
                                        request.requestUrl
                                            ?.encodedPath
                                            ?.endsWith("/api/projects") == true
                                    )
                                        """{"projects":[]}"""
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/agents") ==
                                            true
                                    )
                                        """{"agents":[]}"""
                                    else "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            val origin = server.url("/").toString()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MissionChatVault(),
                    beaconOrigin = "",
                )
            compose.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(origin)
                }
                val state = vm.state.collectAsState().value
                CairnTheme("light") {
                    if (state.installation != null) RunScreen(vm, state, run.id, openRun = {})
                }
            }
            try {
                compose.waitUntil(15000) {
                    compose
                        .onAllNodesWithContentDescription("Options de l’exécution")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithContentDescription("Options de l’exécution").performClick()
                compose.onNodeWithText("Reprendre").performClick()
                compose.waitUntil(10000) { resumes.get() == 1 }
                assertEquals(1, resumes.get())
            } finally {
                vm.api.closeStreams()
            }
        }
    }
}

private class MissionChatVault : SessionVault {
    private val values = mutableMapOf<String, String>()

    override fun read(origin: String) = values[origin]

    override fun write(origin: String, cookie: String?) {
        if (cookie == null) values.remove(origin) else values[origin] = cookie
    }
}
