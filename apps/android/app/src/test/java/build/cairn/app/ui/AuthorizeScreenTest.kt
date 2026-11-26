package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.text.TextRange
import androidx.lifecycle.ViewModelStore
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
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
    fun `sending the first email code starts the resend cooldown on the code screen`() {
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse =
                        when (request.path) {
                            "/api/account/session" ->
                                MockResponse().setBody("""{"authenticated":false}""")
                            "/api/account/options" -> MockResponse().setBody("{}")
                            "/api/account/email-code" ->
                                MockResponse()
                                    .setResponseCode(202)
                                    .setBody("""{"challenge":"first"}""")
                            else -> MockResponse().setResponseCode(404)
                        }
                }
            server.start()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MemoryVault(),
                    beaconOrigin = server.url("/").toString(),
                )
            val models = ViewModelStore().apply { put("sign-in", vm) }
            var mounted by mutableStateOf(true)
            try {
                compose.setContent { if (mounted) CairnTheme { CairnApp(vm = vm) } }
                compose.waitUntil(10_000) {
                    compose.onAllNodes(hasSetTextAction()).fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNode(hasSetTextAction()).performTextInput("owner@example.test")
                compose.onNodeWithText("Recevoir un code").performScrollTo().performClick()
                compose.waitUntil(10_000) {
                    compose
                        .onAllNodesWithText("Vérifiez vos e-mails")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Réessayer dans", substring = true).assertIsNotEnabled()
                compose.onNodeWithText("Se connecter").assertIsNotEnabled()
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
                            "/api/mcp/oauth/preview" -> {
                                val parameters =
                                    wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                                val clientId =
                                    parameters.getValue("client_id").jsonPrimitive.content
                                val clientName =
                                    if (clientId == "client-2") "Autre assistant" else "Claude"
                                MockResponse()
                                    .setBody(
                                        """{"client":{"client_id":"$clientId","client_name":"$clientName","redirect_uri":"https://claude.ai/cb"},"scopes":["read","manage","run"],"installations":[{"id":"inst-3","name":"Atelier","role":"owner","online":true},{"id":"inst-2","name":"Bureau","role":"owner","online":false}],"confirmationRequired":true}"""
                                    )
                            }
                            "/api/account/email-code" ->
                                MockResponse().setBody("""{"challenge":"email-challenge"}""")
                            "/api/account/reauth/email" ->
                                MockResponse()
                                    .setResponseCode(401)
                                    .setBody("""{"error":"Invalid or expired code"}""")
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
            var incomingUrl by mutableStateOf(sharedUrl)
            try {
                var consumed = false
                val restoration = StateRestorationTester(compose)
                restoration.setContent {
                    if (mounted)
                        CairnTheme {
                            CairnApp(
                                sharedUrl = incomingUrl,
                                consumedShare = {
                                    incomingUrl = ""
                                    consumed = true
                                },
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

                compose.onNodeWithText("Changer de lien").performScrollTo().performClick()
                compose
                    .onNode(hasSetTextAction())
                    .performScrollTo()
                    .performTextReplacement(sharedUrl.replace("client-1", "client-2"))
                compose.onNodeWithText("Examiner la demande").performScrollTo().performClick()
                compose.waitUntil(10_000) {
                    compose
                        .onAllNodesWithText("Autoriser « Autre assistant » ?")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.runOnIdle { incomingUrl = sharedUrl }
                compose.waitUntil(10_000) {
                    compose
                        .onAllNodesWithText("Autoriser « Claude » ?")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Autoriser « Autre assistant » ?").assertDoesNotExist()

                compose.runOnIdle { incomingUrl = "https://elsewhere.example.test/authorize" }
                compose.waitUntil(10_000) {
                    compose
                        .onAllNodesWithText("Le lien doit appartenir au serveur", substring = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Autoriser « Claude » ?").assertDoesNotExist()
                compose.onNodeWithText("Autoriser").assertDoesNotExist()

                compose.runOnIdle { incomingUrl = sharedUrl }
                compose.waitUntil(10_000) {
                    compose
                        .onAllNodesWithText("Autoriser « Claude » ?")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Compte Cairn · owner@example.test").assertIsDisplayed()
                compose.onNodeWithText("Client non vérifié").assertIsDisplayed()
                compose.onNodeWithText("https://claude.ai/cb").assertIsDisplayed()
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

                compose
                    .onNodeWithText("Recevoir un code par e-mail")
                    .performScrollTo()
                    .performClick()
                // The code field appears once Beacon answers the code request.
                compose.waitUntil(10_000) {
                    compose.onAllNodes(hasSetTextAction()).fetchSemanticsNodes().size == 1
                }
                val codeInput = compose.onNode(hasSetTextAction())
                codeInput.performTextInput("12")
                compose.onNodeWithText("Valider la confirmation").assertIsNotEnabled()
                codeInput.performTextReplacement("12345678")
                codeInput.performScrollTo().performTouchInput {
                    click(Offset(width * 0.4f, height * 0.8f))
                }
                codeInput.assert(
                    SemanticsMatcher.expectValue(
                        SemanticsProperties.TextSelectionRange,
                        TextRange(3),
                    )
                )
                compose.onNodeWithText("Valider la confirmation").performScrollTo().performClick()
                compose.waitUntil(10_000) {
                    compose
                        .onAllNodesWithText("Invalid or expired code")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                assertTrue(vm.state.value.session.authenticated)
                compose.onNodeWithText("Autoriser « Claude » ?").assertExists()

                compose
                    .onNodeWithText("Confirmer avec une passkey")
                    .performScrollTo()
                    .performClick()
                compose.waitUntil(10_000) {
                    compose
                        .onAllNodesWithText("Identité confirmée")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }

                // Approve consent
                compose.onNodeWithText("Autoriser").performScrollTo().performClick()
                compose.waitUntil(10_000) {
                    compose.onAllNodesWithText("Accès autorisé").fetchSemanticsNodes().isNotEmpty()
                }
                restoration.emulateSavedInstanceStateRestore()
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

    @Test
    fun `a link shared during a decision waits behind that decision's return link`() {
        MockWebServer().use { server ->
            val consents = CopyOnWriteArrayList<String>()
            val release = CountDownLatch(1)
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path.orEmpty().substringBefore('?')
                        return when (path) {
                            "/api/account/session" ->
                                MockResponse()
                                    .setBody(
                                        """{"authenticated":true,"csrf":"csrf","account":{"id":"owner","email":"owner@example.test"},"installations":[{"id":"inst-1","name":"Maison","role":"owner","online":true}]}"""
                                    )
                            "/api/installations" ->
                                MockResponse()
                                    .setBody(
                                        """[{"id":"inst-1","name":"Maison","role":"owner","online":true}]"""
                                    )
                            "/api/mcp/oauth/preview" -> {
                                val clientId =
                                    wireJson
                                        .parseToJsonElement(request.body.readUtf8())
                                        .jsonObject
                                        .getValue("client_id")
                                        .jsonPrimitive
                                        .content
                                val clientName =
                                    if (clientId == "client-2") "Autre assistant" else "Claude"
                                MockResponse()
                                    .setBody(
                                        """{"client":{"client_id":"$clientId","client_name":"$clientName","redirect_uri":"https://claude.ai/cb"},"scopes":["read"],"installations":[{"id":"inst-1","name":"Maison","role":"owner","online":true}],"confirmationRequired":false}"""
                                    )
                            }
                            "/api/mcp/oauth/consent" -> {
                                consents += request.body.readUtf8()
                                release.await(10, TimeUnit.SECONDS)
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
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    vault,
                    beaconOrigin = origin,
                )
            val firstUrl =
                "${origin.trimEnd('/')}/authorize?client_id=client-1&redirect_uri=https%3A%2F%2Fclaude.ai%2Fcb&response_type=code&code_challenge=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&code_challenge_method=S256&scope=read"
            val models = ViewModelStore().apply { put("authorize-busy", vm) }
            var mounted by mutableStateOf(true)
            var incomingUrl by mutableStateOf(firstUrl)
            try {
                compose.setContent {
                    if (mounted)
                        CairnTheme {
                            CairnApp(
                                sharedUrl = incomingUrl,
                                consumedShare = { incomingUrl = "" },
                                vm = vm,
                            )
                        }
                }
                compose.waitUntil(10_000) {
                    compose
                        .onAllNodesWithText("Autoriser « Claude » ?")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Autoriser").performScrollTo().performClick()
                compose.waitUntil(10_000) { consents.isNotEmpty() && vm.state.value.busy }

                val secondUrl = firstUrl.replace("client-1", "client-2")
                compose.runOnIdle { incomingUrl = secondUrl }
                compose.waitForIdle()
                assertEquals(secondUrl, incomingUrl)
                compose.onNodeWithText("Autoriser « Claude » ?").assertExists()

                release.countDown()
                compose.waitUntil(10_000) {
                    compose.onAllNodesWithText("Accès autorisé").fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNodeWithText("Retourner au client").performScrollTo().assertIsDisplayed()
                assertEquals(secondUrl, incomingUrl)

                compose
                    .onNodeWithText("Examiner la nouvelle demande")
                    .performScrollTo()
                    .performClick()
                compose.waitUntil(10_000) {
                    compose
                        .onAllNodesWithText("Autoriser « Autre assistant » ?")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                assertEquals("", incomingUrl)
                assertEquals(1, consents.size)
                assertTrue(consents.first().contains("client-1"))

                // A summary dropped with its dialog, as on sign-out, must not hold later shares.
                compose.onNodeWithText("Autoriser").performScrollTo().performClick()
                compose.waitUntil(10_000) {
                    compose.onAllNodesWithText("Accès autorisé").fetchSemanticsNodes().isNotEmpty()
                }
                compose.runOnIdle { mounted = false }
                compose.waitForIdle()
                compose.runOnIdle { mounted = true }
                compose.waitForIdle()
                compose.runOnIdle { incomingUrl = firstUrl }
                compose.waitUntil(10_000) {
                    compose
                        .onAllNodesWithText("Autoriser « Claude » ?")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                assertEquals("", incomingUrl)
            } finally {
                release.countDown()
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
