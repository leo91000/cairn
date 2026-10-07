package dev.leo.manager.ui

import android.app.Application
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.core.app.ApplicationProvider
import dev.leo.manager.data.*
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule

abstract class NativeAccountCases {
    @get:Rule val compose = createComposeRule()

    open fun sessionVault(application: Application): SessionVault =
        object : SessionVault {
            private val values = mutableMapOf<String, String>()

            override fun read(origin: String) = values[origin]

            override fun write(origin: String, cookie: String?) {
                if (cookie == null) values.remove(origin) else values[origin] = cookie
            }
        }

    fun providerOpensTheLeoAccount(provider: String) {
        val application = ApplicationProvider.getApplicationContext<Application>()
        val vault = sessionVault(application)
        MockWebServer().use { server ->
            val authenticated = java.util.concurrent.atomic.AtomicBoolean(false)
            val exchanges = mutableListOf<String>()
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path.orEmpty()
                        when {
                            path == "/api/account/session" ->
                                return MockResponse()
                                    .setBody(
                                        if (authenticated.get())
                                            """{"authenticated":true,"csrf":"csrf","account":{"id":"account","email":"alice@example.test"},"installations":[]}"""
                                        else """{"authenticated":false}"""
                                    )
                            path == "/api/account/methods" ->
                                return MockResponse()
                                    .setBody(
                                        """{"methods":[{"id":"email","kind":"email","label":"alice@example.test"}]}"""
                                    )
                            path == "/api/installations" -> return MockResponse().setBody("[]")
                            path == "/api/account/options" ->
                                return MockResponse()
                                    .setBody("""{"google":true,"github":true,"passkeys":true}""")
                            path == "/api/account/oauth/google/start" ->
                                return MockResponse()
                                    .setBody(
                                        """{"challenge":"challenge","nonce":"nonce","clientId":"client"}"""
                                    )
                                    .addHeader("Set-Cookie", "leo_oauth=browser; Path=/; HttpOnly")
                            path == "/api/account/oauth/github/start" ->
                                return MockResponse()
                                    .setBody(
                                        """{"challenge":"challenge","secret":"secret","url":"${server.url("/api/account/oauth/github/native/browser?token=launch")}"}"""
                                    )
                            path == "/api/account/passkeys/login/start" ->
                                return MockResponse()
                                    .setBody(
                                        """{"challenge":"challenge","options":{"publicKey":{"rpId":"localhost","challenge":"Y2hhbGxlbmdl","userVerification":"required"}}}"""
                                    )
                                    .addHeader(
                                        "Set-Cookie",
                                        "leo_passkey=browser; Path=/; HttpOnly",
                                    )
                            path.endsWith("/callback") ||
                                path.endsWith("/native/finish") ||
                                path.endsWith("/passkeys/login/finish") -> {
                                exchanges += path
                                authenticated.set(true)
                                return MockResponse()
                                    .setBody(
                                        """{"authenticated":true,"csrf":"csrf","account":{"id":"account","email":"alice@example.test"},"installations":[]}"""
                                    )
                                    .addHeader("Set-Cookie", "leo_session=leo; Path=/; HttpOnly")
                            }
                        }
                        return MockResponse().setBody("{}").setResponseCode(404)
                    }
                }
            server.start()
            val credentials =
                object : LeoCredentials {
                    override suspend fun google(clientId: String, nonce: String): String {
                        assertEquals("client", clientId)
                        assertEquals("nonce", nonce)
                        return "google-proof"
                    }

                    override suspend fun authenticatePasskey(options: String): String {
                        assertTrue(options.contains("rpId"))
                        return """{"id":"key","type":"public-key","response":{"signature":"proof"}}"""
                    }

                    override suspend fun createPasskey(options: String): String = error("unused")

                    override fun openGitHub(url: String) {
                        assertTrue(url.startsWith(server.url("/").toString()))
                        assertFalse(url.contains("secret"))
                    }
                }
            val vm = LeoViewModel(application, vault, server.url("/").toString())
            compose.setContent { LeoTheme { LeoApp(vm = vm, credentials = credentials) } }
            compose.waitUntil(10_000) {
                compose
                    .onAllNodesWithText("Continuer avec $provider")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Continuer avec $provider").performScrollTo().performClick()
            compose.waitUntil(10_000) {
                compose.onAllNodesWithText("Aucune installation").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onAllNodesWithText("alice@example.test").onFirst().assertIsDisplayed()
            assertTrue(authenticated.get())
            assertEquals(1, exchanges.size)
            assertTrue(
                vault.read(server.url("/").toString()).orEmpty().startsWith("leo_session=leo;")
            )
        }
    }

    fun notificationOpensOnlyItsAccessibleInstallation() {
        val application = ApplicationProvider.getApplicationContext<Application>()
        MockWebServer().use { server ->
            val accessible =
                java.util.concurrent.atomic.AtomicReference(
                    """[{"id":"native-home","name":"Maison","role":"owner"},{"id":"native-work","name":"Bureau","role":"member"}]"""
                )
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse =
                        when (request.path) {
                            "/api/account/session" ->
                                MockResponse()
                                    .setBody(
                                        """{"authenticated":true,"csrf":"csrf","account":{"id":"native-person","email":"alice@example.test"},"installations":${accessible.get()}}"""
                                    )
                            "/api/installations" -> MockResponse().setBody(accessible.get())
                            else -> MockResponse().setResponseCode(404).setBody("{}")
                        }
                }
            server.start()
            val origin = server.url("/").toString()
            val vault = sessionVault(application)
            vault.write(origin, "leo_session=native-target; Path=/; Max-Age=3600; HttpOnly")
            val vm = LeoViewModel(application, vault, origin)
            compose.setContent { LeoTheme { LeoApp(vm = vm) } }
            compose.waitUntil(10_000) {
                compose
                    .onAllNodesWithText("Maison · Hors ligne")
                    .fetchSemanticsNodes()
                    .isNotEmpty() && !vm.state.value.busy
            }
            kotlinx.coroutines.runBlocking {
                assertTrue(vm.openNotification(origin + "native-work", "native-person"))
                assertEquals("native-work", vm.state.value.installation?.id)
                assertFalse(vm.openNotification(origin + "native-home", "another-person"))
                assertEquals("native-work", vm.state.value.installation?.id)
                accessible.set("""[{"id":"native-home","name":"Maison","role":"owner"}]""")
                assertFalse(vm.openNotification(origin + "native-work", "native-person"))
                assertEquals("native-home", vm.state.value.installation?.id)
            }
        }
    }
}
