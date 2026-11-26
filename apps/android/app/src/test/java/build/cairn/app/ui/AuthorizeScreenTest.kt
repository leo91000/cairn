package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class AuthorizeScreenTest {
    @get:Rule val compose = createComposeRule()

    @Before
    fun initializeWork() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            ApplicationProvider.getApplicationContext<Application>(),
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
    }

    @After
    fun closeWork() {
        WorkManagerTestInitHelper.closeWorkDatabase()
    }

    @Test
    fun `shared authorization link previews destination scopes confirmation state and records consent`() {
        MockWebServer().use { server ->
            val consents = CopyOnWriteArrayList<String>()
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path.orEmpty().substringBefore('?')
                        return when (path) {
                            "/api/account/session" ->
                                MockResponse()
                                    .setBody(
                                        """{"authenticated":true,"csrf":"csrf","account":{"id":"owner","email":"owner@example.test"},"installations":[{"id":"inst-1","name":"Maison","role":"member","online":false}]}"""
                                    )
                            "/api/installations" ->
                                MockResponse()
                                    .setBody(
                                        """[{"id":"inst-1","name":"Maison","role":"member","online":false}]"""
                                    )
                            "/api/mcp/oauth/preview" ->
                                MockResponse()
                                    .setBody(
                                        """{"client":{"client_id":"client-1","client_name":"Claude","redirect_uri":"https://claude.ai/cb"},"scopes":["read","manage","run"],"installations":[{"id":"inst-3","name":"Atelier","role":"owner","online":true},{"id":"inst-2","name":"Bureau","role":"owner","online":false}],"confirmationRequired":true}"""
                                    )
                            "/api/account/passkeys/reauth/start" ->
                                MockResponse()
                                    .setBody(
                                        """{"challenge":"reauth-ch","options":{"publicKey":{"rpId":"localhost","challenge":"Y2hhbGxlbmdl"}}}"""
                                    )
                            "/api/account/passkeys/reauth/finish" ->
                                MockResponse().setResponseCode(204)
                            "/api/mcp/oauth/consent" -> {
                                consents += request.body.readUtf8()
                                MockResponse()
                                    .setBody("""{"redirect":"https://claude.ai/cb?code=ok"}""")
                            }
                            else -> MockResponse().setResponseCode(404).setBody("{}")
                        }
                    }
                }
            server.start()
            val origin = server.url("/").toString()
            val vault =
                MemoryVault().apply {
                    write(origin, "cairn_session=auth-cookie; Path=/; HttpOnly")
                }
            val credentials =
                object : CairnCredentials {
                    override suspend fun google(clientId: String, nonce: String) = error("unused")

                    override suspend fun authenticatePasskey(options: String) =
                        """{"id":"key","type":"public-key"}"""

                    override suspend fun createPasskey(options: String) = error("unused")

                    override fun openGitHub(url: String) = Unit
                }
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    vault,
                    beaconOrigin = origin,
                )
            val sharedUrl =
                "${origin.trimEnd('/')}/authorize?client_id=client-1&redirect_uri=https%3A%2F%2Fclaude.ai%2Fcb&response_type=code&code_challenge=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&code_challenge_method=S256&scope=read+manage+run"
            val models = ViewModelStore().apply { put("authorize", vm) }
            var mounted by mutableStateOf(true)
            try {
                var consumed = false
                compose.setContent {
                    if (mounted)
                        CairnTheme {
                            CairnApp(
                                sharedUrl = sharedUrl,
                                consumedShare = { consumed = true },
                                vm = vm,
                                credentials = credentials,
                            )
                        }
                }

                compose.waitUntil(10_000) {
                    compose
                        .onAllNodesWithText("Autoriser « Claude » ?")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                assertTrue(consumed)
                compose.onNodeWithText("Client non vérifié").assertIsDisplayed()
                compose.onNodeWithText("claude.ai").assertIsDisplayed()
                compose.onNodeWithText("HTTPS").assertIsDisplayed()
                compose.onNodeWithText("Exécution complète").performScrollTo().assertIsDisplayed()
                compose.onNodeWithText("Accès élevé").assertIsDisplayed()
                compose
                    .onNodeWithText("Confirmation d’identité requise")
                    .performScrollTo()
                    .assertIsDisplayed()
                compose.onNodeWithText("Autoriser").performScrollTo().assertIsNotEnabled()

                // Switch installation to Bureau
                compose.onNodeWithText("Atelier").performScrollTo().performClick()
                compose.onNodeWithText("Bureau").performClick()

                // Confirm identity via passkey inline
                compose
                    .onNodeWithText("Confirmer avec une passkey")
                    .performScrollTo()
                    .performClick()
                compose.waitUntil(10_000) {
                    compose
                        .onAllNodesWithText("Identité confirmée récemment — prêt à autoriser")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }

                // Approve consent
                compose.onNodeWithText("Autoriser").performScrollTo().performClick()
                compose.waitUntil(10_000) {
                    compose.onAllNodesWithText("Accès autorisé").fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNodeWithText("Retourner au client").performScrollTo().assertIsDisplayed()
                assertEquals(1, consents.size)
                val payload = wireJson.parseToJsonElement(consents.first()).jsonObject
                assertEquals("inst-2", payload["installationId"]?.jsonPrimitive?.content)
                assertEquals("true", payload["approved"]?.jsonPrimitive?.content)
            } finally {
                try {
                    compose.runOnIdle { mounted = false }
                    compose.waitForIdle()
                } finally {
                    models.clear()
                }
            }
        }
    }
}
