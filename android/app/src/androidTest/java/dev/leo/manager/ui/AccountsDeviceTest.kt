package dev.leo.manager.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.leo.manager.data.*
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import org.junit.*
import org.junit.Assert.*
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AccountsDeviceTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun claudeAccountSignsInWithAuthorizationCode() {
        MockWebServer().use { server ->
            // The server thread and the test share these.
            val signIn = AtomicReference("null")
            val accounts = AtomicReference("")
            val codes = CopyOnWriteArrayList<String>()
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val result =
                            when ("${request.method} ${request.path?.substringBefore('?')}") {
                                "GET /api/session" -> """{"authenticated":true,"csrf":"fixture"}"""
                                "GET /api/accounts" ->
                                    """{"accounts":[${accounts.get()}],"signIn":${signIn.get()}}"""
                                "POST /api/accounts" -> {
                                    assertEquals("fixture", request.getHeader("X-CSRF-Token"))
                                    signIn.set(
                                        """{"accountId":"studio","provider":"claude","state":"pending","phase":"authorizing","url":"https://claude.ai/oauth/authorize?fixture=1","acceptsCode":true}"""
                                    )
                                    signIn.get()
                                }
                                "POST /api/accounts/sign-in/code" -> {
                                    codes += request.body.readUtf8()
                                    signIn.set(
                                        """{"accountId":"studio","provider":"claude","state":"complete"}"""
                                    )
                                    accounts.set(
                                        """{"id":"studio","provider":"claude","name":"Studio","email":"claude@example.test","state":"ready","status":"next"}"""
                                    )
                                    "{}"
                                }
                                "GET /api/overview",
                                "GET /api/codex/models",
                                "GET /api/claude/models" -> "{}"
                                else -> "[]"
                            }
                        return MockResponse().setBody(result)
                    }
                }
            server.start()
            val vm =
                LeoViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    DeviceAccountsVault(),
                )
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                LeoTheme { if (state.session.authenticated) ConnectionsScreen(vm, state) }
            }
            compose.waitUntil(15000) {
                compose
                    .onAllNodesWithContentDescription("Ajouter un compte Claude Code")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithContentDescription("Ajouter un compte Claude Code").performClick()
            compose.onNodeWithText("Continuer vers la connexion").performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Code d’autorisation Claude")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Code d’autorisation Claude").performTextInput("fixture-code")
            compose.onNodeWithText("Terminer la connexion").performScrollTo().performClick()
            compose.waitUntil(10000) { codes.isNotEmpty() }
            assertEquals("""{"code":"fixture-code"}""", codes.single())
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("claude@example.test").fetchSemanticsNodes().isNotEmpty()
            }
        }
    }

    @Test
    fun providerSerializationPreservesExistingAgents() {
        val old = wireJson.decodeFromString<Agent>("""{"name":"Existing"}""")
        assertEquals("codex", old.provider)
        val claude = old.copy(provider = "claude", model = "sonnet", reasoning = "high")
        assertEquals(claude, wireJson.decodeFromString<Agent>(wireJson.encodeToString(claude)))
    }
}

private class DeviceAccountsVault : SessionVault {
    private val values = mutableMapOf<String, String>()

    override fun read(origin: String) = values[origin]

    override fun write(origin: String, cookie: String?) {
        if (cookie == null) values.remove(origin) else values[origin] = cookie
    }
}
