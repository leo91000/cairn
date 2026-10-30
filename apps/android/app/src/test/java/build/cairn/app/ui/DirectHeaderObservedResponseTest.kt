package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import build.cairn.app.data.*
import java.nio.ByteBuffer
import java.util.Base64
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.*
import org.junit.Assert.*
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import org.robolectric.shadows.ShadowLooper

/** The real selected ViewModel and header observe responses, rather than peer readiness. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class DirectHeaderObservedResponseTest {
    @get:Rule val compose = createComposeRule()
    private val application = ApplicationProvider.getApplicationContext<Application>()
    private val models = ViewModelStore()

    @Before
    fun setup() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            application,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        runBlocking { NotificationPreferences(application).setEnabled(false) }
    }

    @After
    fun cleanup() {
        models.clear()
        WorkManagerTestInitHelper.closeWorkDatabase()
    }

    private fun workspaceBody(path: String) =
        when (path.substringBefore('?').substringAfterLast('/')) {
            "agents" -> """{"agents":[]}"""
            "projects" -> """{"projects":[]}"""
            "tasks" -> """{"tasks":[]}"""
            "mcps" -> """{"mcps":[]}"""
            "skills" -> "[]"
            else -> "{}"
        }

    @Test
    fun `a late peer needs a response through the selected ViewModel before its header is direct`() {
        MockWebServer().use { server ->
            val relayReads = AtomicInteger()
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.requestUrl!!.encodedPath
                        if (path.startsWith("/api/installations/fixture/api/"))
                            relayReads.incrementAndGet()
                        val result =
                            when (path) {
                                "/api/account/session" -> beaconAccountFixture()
                                "/api/installations" -> beaconInstallationsFixture()
                                else -> workspaceBody(path)
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(result)
                    }
                }
            server.start()
            val origin = server.url("/").toString()
            val vm =
                CairnViewModel(
                    application,
                    object : SessionVault {
                        override fun read(origin: String): String? = null

                        override fun write(origin: String, cookie: String?) {}
                    },
                    beaconOrigin = "",
                )
            models.put("direct-header", vm)
            compose.setContent {
                val state by vm.state.collectAsState()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(origin)
                }
                CairnTheme { InstallationHeader(state, openConnection = {}) }
            }
            compose.waitUntil(30000) {
                vm.state.value.let {
                    it.ready && !it.busy && it.installation?.id == "fixture"
                }
            }
            compose.onNode(installationHeader(state = "En ligne · Relais")).assertIsDisplayed()
            val requests = AtomicInteger()
            val responses = AtomicInteger()
            lateinit var channel: DirectChannel
            channel =
                DirectChannel(
                    { packet, _, _ ->
                        val request =
                            wireJson
                                .parseToJsonElement(String(packet.copyOfRange(13, packet.size)))
                                .jsonObject
                        assertEquals("request", request["type"]!!.jsonPrimitive.content)
                        requests.incrementAndGet()
                        val content = workspaceBody(request["path"]!!.jsonPrimitive.content)
                        val response = buildJsonObject {
                            put("type", "response")
                            put("id", request["id"]!!)
                            put("status", 200)
                            put(
                                "headers",
                                buildJsonArray {
                                    add(
                                        buildJsonArray {
                                            add("content-type")
                                            add("application/json")
                                        }
                                    )
                                },
                            )
                            put("body", Base64.getEncoder().encodeToString(content.toByteArray()))
                        }
                            .toString()
                            .toByteArray()
                        channel.receive(
                            ByteBuffer.allocate(13 + response.size)
                                .put(1)
                                .putInt(responses.incrementAndGet())
                                .putInt(response.size)
                                .putInt(0)
                                .put(response)
                                .array()
                        )
                    },
                    {},
                )
            compose.runOnUiThread {
                // The native peer has a separate real-device gate; control only its arrival here.
                vm.api.closeStreams()
                vm.api.transport.attach(channel)
            }
            compose.waitForIdle()
            assertEquals(0, requests.get())
            assertEquals("relay", vm.api.transport.route.value)
            compose.onNode(installationHeader(state = "En ligne · Relais")).assertIsDisplayed()
            val relayRequests = relayReads.get()

            // This is the production workspace refresh action, using this same selected VM/API.
            compose.runOnUiThread { vm.perform { refresh() } }
            compose.waitUntil(30000) {
                // ViewModelScope resumes through the platform Handler, outside Compose's clock.
                ShadowLooper.idleMainLooper()
                vm.state.value.let { !it.busy && it.transportRoute == "direct" }
            }
            compose.onNode(installationHeader(state = "En ligne · Direct")).assertIsDisplayed()
            assertNull(vm.state.value.error)
            assertTrue(requests.get() >= 7)
            assertEquals(requests.get(), responses.get())
            assertEquals(relayRequests, relayReads.get())
        }
    }
}
