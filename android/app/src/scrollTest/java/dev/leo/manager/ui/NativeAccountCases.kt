package dev.leo.manager.ui

import android.app.Application
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.datastore.preferences.core.edit
import androidx.test.core.app.ApplicationProvider
import dev.leo.manager.data.*
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.first
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

    fun refusedPushRenewalTurnsOffAndExplainsRecovery() {
        val application = ApplicationProvider.getApplicationContext<Application>()
        val preferences = NotificationPreferences(application)
        val vault = sessionVault(application)
        val registered = java.util.concurrent.atomic.AtomicBoolean(true)
        val registrationAttempts = java.util.concurrent.atomic.AtomicInteger()
        val installations = java.util.concurrent.atomic.AtomicReference("[]")
        val recoveryNotice =
            "Les notifications push ont été désactivées. Confirmez votre identité par e-mail ou passkey dans les réglages du compte, puis réactivez-les."
        val pushToggle = hasText("Notifications push")
        val tokens =
            object : PushTokens {
                override val available = true

                override suspend fun token() = "device-token"

                override suspend fun delete() = Unit
            }
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse =
                        when {
                            request.path == "/api/account/session" ->
                                MockResponse()
                                    .setBody(
                                        """{"authenticated":true,"csrf":"csrf","account":{"id":"push-person","email":"alice@example.test"},"installations":${installations.get()}}"""
                                    )
                            request.path == "/api/installations" ->
                                MockResponse().setBody(installations.get())
                            request.path == "/api/account/notifications/android" &&
                                request.method == "GET" ->
                                MockResponse().setBody("""{"enabled":true}""")
                            request.path ==
                                "/api/account/notifications/subscriptions/push-registration" ->
                                MockResponse().setBody("""{"registered":${registered.get()}}""")
                            request.path == "/api/account/notifications/android" &&
                                request.method == "POST" -> {
                                registrationAttempts.incrementAndGet()
                                if (registered.get())
                                    MockResponse().setBody("""{"id":"push-registration"}""")
                                else
                                    MockResponse()
                                        .setResponseCode(403)
                                        .setBody("""{"error":"Confirm identity"}""")
                            }
                            else -> MockResponse().setResponseCode(404).setBody("{}")
                        }
                }
            server.start()
            val origin = server.url("/").toString()
            vault.write(origin, "leo_session=push-fixture; Path=/; Max-Age=3600; HttpOnly")
            val registrar = NativeDeviceRegistrar(application, vault, origin, tokens)
            kotlinx.coroutines.runBlocking { registrar.enable() }
            val vm = LeoViewModel(application, vault, origin)
            val currentVm = mutableStateOf(vm)
            val showingHome = mutableStateOf(false)
            lateinit var pushScope: CoroutineScope
            compose.setContent {
                pushScope = rememberCoroutineScope()
                LeoTheme {
                    if (showingHome.value) LeoApp(vm = currentVm.value)
                    else NotificationSettings(currentVm.value)
                }
            }

            fun runPushAction(action: suspend () -> Unit) {
                val operation = pushScope.async { action() }
                try {
                    // Keep Compose's scheduler moving: a foreground DataStore edit may hold
                    // its write lock while waiting to resume on this same scheduler.
                    compose.waitUntil(10_000) { operation.isCompleted }
                    kotlinx.coroutines.runBlocking { operation.await() }
                } finally {
                    operation.cancel()
                }
            }

            compose.waitUntil(10_000) {
                compose.onAllNodes(pushToggle and isOn()).fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Notifications push").assertIsOn()

            // Reproduce a foreground preference write whose continuation needs Compose's
            // scheduler while the background renewal is handling its refusal.
            val preferenceWriteStarted = CompletableDeferred<Unit>()
            val finishPreferenceWrite = CompletableDeferred<Unit>()
            val preferenceWrite = pushScope.async {
                application.dataStore.edit {
                    preferenceWriteStarted.complete(Unit)
                    finishPreferenceWrite.await()
                }
            }
            compose.waitUntil(10_000) { preferenceWriteStarted.isCompleted }
            finishPreferenceWrite.complete(Unit)

            registered.set(false)
            runPushAction {
                try {
                    registrar.register()
                } catch (error: ApiException) {
                    assertEquals(403, error.status)
                }
                assertFalse(preferences.enabled.first())
                assertTrue(NotificationPreferences(application).nativeReenrollmentRequired.first())
                // A later background token callback must not silently re-enroll this device.
                registrar.register()
                preferenceWrite.await()
            }
            assertEquals(2, registrationAttempts.get())
            compose.waitUntil(10_000) {
                compose.onAllNodesWithText(recoveryNotice).fetchSemanticsNodes().isNotEmpty() &&
                    compose.onAllNodes(pushToggle and isOff()).fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Notifications push").assertIsOff()
            compose.onNodeWithText(recoveryNotice).assertIsDisplayed()

            // Reopening the app must explain the disabled push even with an offline installation.
            installations.set(
                """[{"id":"push-home","name":"Maison","role":"owner","online":false}]"""
            )
            compose.runOnIdle {
                currentVm.value = LeoViewModel(application, vault, origin)
                showingHome.value = true
            }
            compose.waitUntil(10_000) {
                compose
                    .onAllNodesWithText("Maison · Hors ligne")
                    .fetchSemanticsNodes()
                    .isNotEmpty() &&
                    compose.onAllNodesWithText(recoveryNotice).fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText(recoveryNotice).assertIsDisplayed()
            compose.runOnIdle { showingHome.value = false }

            registered.set(true)
            runPushAction {
                registrar.enable()
                assertTrue(preferences.enabled.first())
            }
            compose.waitUntil(10_000) {
                compose.onAllNodesWithText(recoveryNotice).fetchSemanticsNodes().isEmpty() &&
                    compose.onAllNodes(pushToggle and isOn()).fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Notifications push").assertIsOn()
            assertEquals(3, registrationAttempts.get())
            runPushAction { registrar.disable() }
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
