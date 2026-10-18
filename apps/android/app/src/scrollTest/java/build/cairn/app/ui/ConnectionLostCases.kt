package build.cairn.app.ui

import android.app.Application
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

/** A lost connection is a quiet notice that leaves on its own, not an error to dismiss. */
abstract class ConnectionLostCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun connectionFailuresAreClassifiedApartFromErrorsTheUserMustRead() {
        assertTrue(isConnectionFailure(ConnectionException(java.net.ConnectException("refused"))))
        assertTrue(isConnectionFailure(ApiException(503, "Installation unavailable")))
        assertTrue(isConnectionFailure(ApiException(503, "Installation busy.")))
        assertTrue(isConnectionFailure(ApiException(504, "Installation request timed out.")))
        assertTrue(isConnectionFailure(ApiException(504, "Installation response timed out.")))
        assertFalse(isConnectionFailure(java.io.IOException("Stockage plein")))
        assertFalse(isConnectionFailure(java.io.FileNotFoundException("Document introuvable")))
        assertTrue(isConnectionFailure(ApiException(502, "Installation connection lost")))
        assertTrue(isConnectionFailure(ApiException(504, "Le serveur…", explained = false)))
        assertFalse(isConnectionFailure(ApiException(502, "GitHub indisponible")))
        assertFalse(isConnectionFailure(ApiException(503, "Offline")))
        assertFalse(isConnectionFailure(ApiException(500, "Erreur", explained = false)))
        assertFalse(isConnectionFailure(IllegalStateException("Choisissez une installation.")))
    }

    @Test
    fun theNoticeClearsItselfAndReloadsTheWorkspaceOnceTheInstallationAnswers() {
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
                    object : SessionVault {
                        override fun read(origin: String): String? = null

                        override fun write(origin: String, cookie: String?) {}
                    },
                    beaconOrigin = "",
                )
            val models = ViewModelStore().apply { put("fixture", vm) }
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
                waitFor {
                    !vm.state.value.connectionLost &&
                        !vm.state.value.busy &&
                        overviews.get() >= before + 2
                }
                compose.waitForIdle()
                compose.onNodeWithText("Connexion perdue · reconnexion…").assertDoesNotExist()
                assertNull(vm.state.value.error)

                // Errors the user must read keep their compact notice.
                compose.runOnIdle { vm.report(ApiException(502, "GitHub indisponible")) }
                compose.onNodeWithText("GitHub indisponible").assertExists()
                compose.onNodeWithContentDescription("Masquer l’erreur").performClick()
                compose.onNodeWithText("GitHub indisponible").assertDoesNotExist()

                compose.runOnIdle { vm.perform { throw java.io.IOException("Stockage plein") } }
                waitFor { !vm.state.value.busy && vm.state.value.error == "Stockage plein" }
                assertFalse(vm.state.value.connectionLost)
                compose.onNodeWithText("Stockage plein").assertExists()
            } finally {
                models.clear()
            }
        }
    }

    @Test
    fun recoveryBelongsToTheNewInstallationAfterSwitching() {
        val firstDown = java.util.concurrent.atomic.AtomicBoolean(false)
        val secondDown = java.util.concurrent.atomic.AtomicBoolean(true)
        val firstProbe = CountDownLatch(1)
        val releaseFirst = CountDownLatch(1)
        val secondOverviews = AtomicInteger()
        val installations =
            """[{"id":"fixture","name":"Première","role":"owner","online":true},{"id":"second","name":"Deuxième","role":"owner","online":true}]"""
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (firstDown.get() && path.startsWith("/api/installations/fixture/")) {
                            if (path.endsWith("/overview")) {
                                firstProbe.countDown()
                                releaseFirst.await(20, TimeUnit.SECONDS)
                            }
                            return unavailable()
                        }
                        if (path.startsWith("/api/installations/second/")) {
                            if (path.endsWith("/overview")) secondOverviews.incrementAndGet()
                            if (secondDown.get()) return unavailable()
                        }
                        val body =
                            when {
                                path == "/api/account/session" ->
                                    """{"authenticated":true,"csrf":"fixture","account":{"id":"fixture-account","email":"owner@example.test"},"installations":$installations}"""
                                path == "/api/installations" -> installations
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
                                else -> "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            val vm = connect(server)
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                firstDown.set(true)
                compose.runOnIdle { vm.perform { refresh() } }
                waitFor { vm.state.value.connectionLost && !vm.state.value.busy }
                waitFor { firstProbe.count == 0L }
                compose.runOnIdle { vm.perform { selectInstallation("second") } }
                waitFor {
                    vm.state.value.installation?.id == "second" &&
                        vm.state.value.connectionLost &&
                        !vm.state.value.busy
                }
                val before = secondOverviews.get()
                secondDown.set(false)
                // The old probe remains blocked; the second installation needs its own recovery.
                waitFor { !vm.state.value.connectionLost && secondOverviews.get() >= before + 2 }
                assertNull(vm.state.value.error)
            } finally {
                releaseFirst.countDown()
                models.clear()
            }
        }
    }

    @Test fun aSuccessfulProbeWaitsForTheUserActionBeforeReloading() = recoverDuringAction(null)

    @Test
    fun recoveryPreservesAMeaningfulErrorFromTheUserAction() =
        recoverDuringAction(java.io.IOException("Stockage plein"))

    private fun recoverDuringAction(failure: Exception?) {
        val overviews = AtomicInteger()
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (path.endsWith("/overview")) overviews.incrementAndGet()
                        val body =
                            when {
                                path == "/api/account/session" -> beaconAccountFixture()
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
                                else -> "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            val vm = connect(server)
            val models = ViewModelStore().apply { put("fixture", vm) }
            val action = CompletableDeferred<Unit>()
            try {
                val before = overviews.get()
                compose.runOnIdle {
                    vm.report(ApiException(503, "Installation busy."))
                    vm.perform { action.await() }
                }
                waitFor { !vm.state.value.connectionLost && overviews.get() > before }
                assertTrue(vm.state.value.busy)
                if (failure == null) action.complete(Unit)
                else action.completeExceptionally(failure)
                waitFor { !vm.state.value.busy && overviews.get() >= before + 2 }
                assertEquals(failure?.message, vm.state.value.error)
            } finally {
                action.complete(Unit)
                models.clear()
            }
        }
    }

    @Test
    fun aFailedAnswerRemainsVisibleInsideTheQuestionDialogWithoutReplayingIt() {
        val down = AtomicBoolean(false)
        val answers = AtomicInteger()
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (path.endsWith("/answer")) answers.incrementAndGet()
                        if (down.get() && path.startsWith("/api/installations/fixture/"))
                            return unavailable()
                        val body =
                            when {
                                path == "/api/account/session" -> beaconAccountFixture()
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
                                else -> "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            val question =
                ChatQuestion(
                    "question",
                    "chat",
                    fields = listOf(QuestionField("answer", "Votre choix")),
                )
            val vm =
                connect(server) { model, state ->
                    CompositionLocalProvider(
                        LocalConnectionFeedback provides
                            ConnectionFeedback(state.connectionLost, state.notice)
                    ) {
                        if (state.installation != null)
                            QuestionDialog(model, state, question, listOf(question), {}, {})
                    }
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                compose.onNodeWithText("Réponse").performTextInput("Conserver cette réponse")
                down.set(true)
                compose.onNodeWithText("Envoyer la réponse").performClick()
                waitFor { vm.state.value.connectionLost && !vm.state.value.busy }
                compose.onNodeWithText("Connexion impossible. Réessayez.").assertIsDisplayed()
                compose.onNodeWithText("Connexion perdue · reconnexion…").assertIsDisplayed()
                compose.onNodeWithText("Conserver cette réponse").assertExists()
                assertNull(vm.state.value.error)
                down.set(false)
                waitFor { !vm.state.value.connectionLost && !vm.state.value.busy }
                compose.waitForIdle()
                compose.onNodeWithText("Connexion perdue · reconnexion…").assertDoesNotExist()
                compose.onNodeWithText("Conserver cette réponse").assertExists()
                assertEquals(1, answers.get())
            } finally {
                models.clear()
            }
        }
    }

    @Test
    fun anOfflineInstallationDoesNotLeaveAPersistentErrorAfterTheDeviceReconnects() {
        val down = AtomicBoolean(false)
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        if (down.get()) return unavailable()
                        val path = request.path!!.substringBefore('?')
                        val body =
                            when {
                                path == "/api/account/session" ->
                                    beaconAccountFixture()
                                        .replace("\"online\":true", "\"online\":false")
                                path == "/api/installations" ->
                                    beaconInstallationsFixture()
                                        .replace("\"online\":true", "\"online\":false")
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
                                else -> "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            val vm = connect(server) { model, _ -> CairnApp(vm = model) }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                assertEquals(false, vm.state.value.installation?.online)
                compose.onNodeWithText("Installation hors ligne").assertExists()
                down.set(true)
                compose.runOnIdle { vm.perform { refreshInstallations() } }
                waitFor { !vm.state.value.busy }
                assertNull(vm.state.value.error)
                compose.onNodeWithText("Connexion perdue · reconnexion…").assertIsDisplayed()
                down.set(false)
                waitFor { !vm.state.value.connectionLost && !vm.state.value.busy }
                compose.waitForIdle()
                assertNull(vm.state.value.error)
                compose.onNodeWithText("Connexion perdue · reconnexion…").assertDoesNotExist()
                compose.onNodeWithText("Installation hors ligne").assertExists()
                compose.mainClock.advanceTimeBy(6000)
                waitFor { vm.state.value.notice == null }
                compose.onNodeWithText("Connexion impossible. Réessayez.").assertDoesNotExist()
            } finally {
                models.clear()
            }
        }
    }

    @Test
    fun aFailedSaveStaysVisibleAfterScrollingTheEditor() {
        val down = AtomicBoolean(false)
        val saves = AtomicInteger()
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (request.method == "POST" && path.endsWith("/tasks"))
                            saves.incrementAndGet()
                        if (down.get()) return unavailable()
                        val body =
                            when {
                                path == "/api/account/session" -> beaconAccountFixture()
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
                                else -> "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            val vm =
                connect(server) { model, state ->
                    CompositionLocalProvider(
                        LocalConnectionFeedback provides
                            ConnectionFeedback(state.connectionLost, state.notice)
                    ) {
                        if (state.installation != null)
                            Editor(
                                "Formulaire long",
                                state.busy,
                                state.error,
                                {},
                                {
                                    model.perform {
                                        api.request(
                                            "POST",
                                            "/tasks",
                                            kotlinx.serialization.json.buildJsonObject {},
                                        )
                                    }
                                },
                            ) {
                                repeat(40) { Text("Champ $it") }
                            }
                    }
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                compose.onNodeWithText("Champ 39").performScrollTo().assertIsDisplayed()
                down.set(true)
                compose.onNodeWithText("Enregistrer").performClick()
                waitFor { vm.state.value.connectionLost && !vm.state.value.busy }
                compose.onNodeWithText("Champ 0").assertIsNotDisplayed()
                compose.onNodeWithText("Connexion impossible. Réessayez.").assertIsDisplayed()
                compose.onNodeWithText("Connexion perdue · reconnexion…").assertIsDisplayed()
                assertNull(vm.state.value.error)
                assertEquals(1, saves.get())
            } finally {
                models.clear()
            }
        }
    }

    private fun unavailable() =
        MockResponse().setResponseCode(503).setBody("""{"error":"Installation busy."}""")

    private fun connect(
        server: MockWebServer,
        content: @Composable (CairnViewModel, Workspace) -> Unit = { _, state ->
            ConnectionLostNotice(state.connectionLost)
        },
    ): CairnViewModel {
        val origin = server.url("/").toString()
        val vault =
            object : SessionVault {
                override fun read(origin: String): String? = null

                override fun write(origin: String, cookie: String?) {}
            }
        val vm =
            CairnViewModel(ApplicationProvider.getApplicationContext(), vault, beaconOrigin = "")
        compose.setContent {
            LaunchedEffect(Unit) {
                vm.state.first { it.ready }
                vm.connect(origin)
            }
            val state = vm.state.collectAsState().value
            CairnTheme { content(vm, state) }
        }
        waitFor { vm.state.value.installation != null && !vm.state.value.busy }
        return vm
    }

    private fun waitFor(condition: () -> Boolean) =
        compose.waitUntil(20000) {
            compose.onAllNodes(isRoot()).fetchSemanticsNodes()
            condition()
        }
}
