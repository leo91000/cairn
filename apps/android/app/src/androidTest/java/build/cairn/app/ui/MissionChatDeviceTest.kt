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
import okhttp3.mockwebserver.*
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class MissionChatDeviceTest {
    @get:Rule val compose = createComposeRule()

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
                                else -> "[]"
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
}

private class MissionChatVault : SessionVault {
    private val values = mutableMapOf<String, String>()

    override fun read(origin: String) = values[origin]

    override fun write(origin: String, cookie: String?) {
        if (cookie == null) values.remove(origin) else values[origin] = cookie
    }
}
