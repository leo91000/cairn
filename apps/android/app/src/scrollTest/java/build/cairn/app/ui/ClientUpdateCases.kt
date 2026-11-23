package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import build.cairn.app.update.UpdateViewModel
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test

/** Real stream refusal reaches the app shell without offering an empty conversation. */
abstract class ClientUpdateCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun incompatibleTimelineOpensTheRequiredUpdateScreenAndSignedAppChannel() {
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.requestUrl!!.encodedPath
                        if (path.endsWith("/chats/incompatible/stream")) {
                            assertEquals("1", request.requestUrl!!.queryParameter("timeline"))
                            return MockResponse()
                                .setResponseCode(426)
                                .setBody(
                                    """{"code":"update_required","error":"Update required."}"""
                                )
                        }
                        val body =
                            when (path) {
                                "/api/account/session" -> beaconAccountFixture()
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/installations/fixture/api/overview" ->
                                    installationOverviewFixture()
                                else ->
                                    when (path.substringAfterLast('/')) {
                                        "agents" -> """{"agents":[]}"""
                                        "projects" -> """{"projects":[]}"""
                                        "tasks" -> """{"tasks":[]}"""
                                        "mcps" -> """{"mcps":[]}"""
                                        "authorize" -> """{"available":false}"""
                                        else -> "[]"
                                    }
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val application = ApplicationProvider.getApplicationContext<Application>()
            val vault =
                object : SessionVault {
                    override fun read(origin: String): String? = null

                    override fun write(origin: String, cookie: String?) {}
                }
            val vm = CairnViewModel(application, vault, beaconOrigin = "")
            val updates = UpdateViewModel(application)
            val models =
                ViewModelStore().apply {
                    put("workspace", vm)
                    put("updates", updates)
                }
            val mounted = mutableStateOf(true)
            try {
                compose.setContent {
                    if (mounted.value) {
                        LaunchedEffect(Unit) {
                            vm.state.first { it.ready }
                            vm.connect(server.url("/").toString())
                            vm.api.live("/chats/incompatible/stream").collect {}
                        }
                        CompositionLocalProvider(LocalAppUpdates provides updates) {
                            CairnTheme { CairnApp(vm = vm) }
                        }
                    }
                }
                compose.waitUntil(15000) {
                    compose
                        .onAllNodesWithText("Mise à jour requise")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Mise à jour requise").assertIsDisplayed()
                compose
                    .onNodeWithText("Rechercher une mise à jour")
                    .performScrollTo()
                    .assertIsDisplayed()
                    .assertIsEnabled()
                compose.onNodeWithText("canal officiel", substring = true).assertExists()
                compose.onNodeWithText("Message", substring = false).assertDoesNotExist()
            } finally {
                compose.runOnIdle {
                    mounted.value = false
                    models.clear()
                }
                compose.waitForIdle()
            }
        }
    }
}
