package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/** A lost connection is a quiet notice that leaves on its own, not an error to dismiss. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class ConnectionLostTest {
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

    @Test
    fun `connection failures are classified apart from errors the user must read`() {
        assertTrue(isConnectionFailure(java.net.ConnectException("refused")))
        assertTrue(isConnectionFailure(ApiException(503, "Installation unavailable")))
        assertTrue(isConnectionFailure(ApiException(502, "Installation connection lost")))
        assertTrue(isConnectionFailure(ApiException(504, "Le serveur…", explained = false)))
        assertFalse(isConnectionFailure(ApiException(502, "GitHub indisponible")))
        assertFalse(isConnectionFailure(ApiException(503, "Offline")))
        assertFalse(isConnectionFailure(ApiException(500, "Erreur", explained = false)))
        assertFalse(isConnectionFailure(IllegalStateException("Choisissez une installation.")))
    }

    @Test
    fun `the notice clears itself and reloads the workspace once the installation answers`() {
        val down = AtomicBoolean(false)
        val overviews = AtomicInteger()
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (path.startsWith("/api/installations/fixture/") && down.get())
                            return MockResponse()
                                .setResponseCode(503)
                                .setBody("""{"error":"Installation unavailable"}""")
                        if (path.endsWith("/overview")) overviews.incrementAndGet()
                        val response =
                            when {
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
                                else -> "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(response)
                    }
                }
            val origin = server.url("/").toString()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MemoryVault(),
                    beaconOrigin = "",
                )
            compose.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(origin)
                }
                CairnTheme {
                    val state = vm.state.collectAsState().value
                    androidx.compose.foundation.layout.Column {
                        ConnectionLostNotice(state.connectionLost)
                        state.error?.let { ErrorNotice(it, vm::clearMessage) }
                    }
                }
            }
            fun waitFor(condition: () -> Boolean) =
                compose.waitUntil(20000) {
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    condition()
                }
            try {
                waitFor { vm.state.value.installation != null && !vm.state.value.busy }
                down.set(true)
                compose.runOnIdle { vm.perform { refresh() } }
                waitFor { vm.state.value.connectionLost && !vm.state.value.busy }
                assertNull(vm.state.value.error)
                compose.onNodeWithText("Connexion perdue · reconnexion…").assertExists()
                compose.onNodeWithContentDescription("Masquer l’erreur").assertDoesNotExist()

                val before = overviews.get()
                down.set(false)
                waitFor { !vm.state.value.connectionLost && overviews.get() >= before + 2 }
                compose.waitForIdle()
                compose.onNodeWithText("Connexion perdue · reconnexion…").assertDoesNotExist()
                assertNull(vm.state.value.error)

                // Errors the user must read keep their compact notice.
                compose.runOnIdle { vm.report(ApiException(502, "GitHub indisponible")) }
                compose.onNodeWithText("GitHub indisponible").assertExists()
                compose.onNodeWithContentDescription("Masquer l’erreur").performClick()
                compose.onNodeWithText("GitHub indisponible").assertDoesNotExist()
            } finally {
                vm.api.closeStreams()
            }
        }
    }
}
