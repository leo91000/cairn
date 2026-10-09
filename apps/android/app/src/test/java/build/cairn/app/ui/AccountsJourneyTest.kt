package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.*
import build.cairn.app.data.*
import java.io.File
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.*
import org.junit.Assert.*
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class AccountsJourneyTest {
    @get:Rule val compose = createComposeRule()

    @Before
    fun setup() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            ApplicationProvider.getApplicationContext<Application>(),
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
    }

    @After
    fun cleanup() {
        WorkManagerTestInitHelper.closeWorkDatabase()
    }

    private val work =
        """{"id":"work","provider":"codex","name":"Work","email":"work@example.test","plan":"plus","state":"ready","status":"next","stale":false,"remainingPercent":82.0,"activeRunIds":["run"],"maxConcurrentRuns":4,"usage":{"checkedAt":1,"windows":[{"id":"main:primary","label":"5-hour window","usedPercent":4,"durationMins":300,"resetsAt":1893499200},{"id":"main:secondary","label":"Weekly","usedPercent":18,"durationMins":10080,"resetsAt":1894017600}],"resets":{"available":3}}}"""

    /** A server whose accounts change with each request, recording what the app sends. */
    private class Accounts {
        val requests = CopyOnWriteArrayList<Pair<String, String>>()
        @Volatile var accounts = mutableListOf<String>()
        @Volatile var signIn = "null"
    }

    private fun journey(
        server: Accounts,
        respond: (RecordedRequest, String) -> String?,
        body: (CairnViewModel) -> Unit,
    ) {
        MockWebServer().use { mock ->
            mock.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val payload = request.body.readUtf8()
                        if (path.startsWith("/api/installations/fixture/api/accounts"))
                            server.requests += "${request.method} $path" to payload
                        val result =
                            respond(request, payload)
                                ?: when (path) {
                                    "/api/installations" -> officialInstallationsFixture()
                                    "/api/account/session" -> officialAccountFixture("fixture")
                                    "/api/installations/fixture/api/accounts" ->
                                        """{"accounts":[${server.accounts.joinToString(",")}],"signIn":${server.signIn}}"""
                                    "/api/installations/fixture/api/connections" ->
                                        """[{"provider":"github","installed":true,"connected":true,"account":"cairn-coletta"}]"""
                                    "/api/installations/fixture/api/connections/login" -> "null"
                                    "/api/installations/fixture/api/runs" ->
                                        """[{"id":"run","status":"running","taskName":"Revue des dépendances"}]"""
                                    "/api/installations/fixture/api/overview",
                                    "/api/installations/fixture/api/codex/models",
                                    "/api/installations/fixture/api/claude/models" -> "{}"
                                    else -> "[]"
                                }
                        return MockResponse().setBody(result)
                    }
                }
            mock.start()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MemoryVault(),
                    officialOrigin = "",
                )
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(mock.url("/").toString())
                }
                CairnTheme("light") { if (state.session.authenticated) ConnectionsScreen(vm, state) }
            }
            body(vm)
        }
    }

    private fun waitText(value: String) =
        compose.waitUntil(15000) {
            compose.onAllNodesWithText(value, substring = true).fetchSemanticsNodes().isNotEmpty()
        }

    private fun capture(name: String) {
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
        compose.waitForIdle()
        File(dir).mkdirs()
        compose
            .onRoot()
            .captureToImage()
            .asAndroidBitmap()
            .compress(
                android.graphics.Bitmap.CompressFormat.PNG,
                100,
                File(dir, "$name.png").outputStream(),
            )
    }

    @Test
    fun `Claude signs in with the shared sheet, sending only its one-time code`() {
        val server = Accounts().apply { accounts += work }
        journey(
            server,
            { request, payload ->
                when ("${request.method} ${request.path}") {
                    "POST /api/installations/fixture/api/accounts" -> {
                        assertEquals("fixture", request.getHeader("X-CSRF-Token"))
                        server.signIn =
                            """{"accountId":"studio","provider":"claude","state":"pending","phase":"authorizing","url":"https://claude.com/oauth/authorize?fixture=1","acceptsCode":true}"""
                        server.accounts +=
                            """{"id":"studio","provider":"claude","name":"Studio","state":"pending","status":"signIn"}"""
                        server.signIn
                    }
                    "POST /api/installations/fixture/api/accounts/sign-in/code" -> {
                        server.signIn =
                            """{"accountId":"studio","provider":"claude","state":"complete","phase":"verifying"}"""
                        server.accounts[1] =
                            """{"id":"studio","provider":"claude","name":"Studio","email":"studio@example.test","plan":"max","state":"ready","status":"next","stale":false,"remainingPercent":40.0,"usage":{"checkedAt":1,"windows":[{"id":"five_hour","label":"5-hour window","usedPercent":25,"durationMins":300},{"id":"seven_day","label":"Weekly","usedPercent":60,"durationMins":10080}]}}"""
                        """{"submitted":true}"""
                    }
                    else -> null
                }
            },
        ) {
            waitText("AGENTS DE CODE")
            waitText("Work")
            compose.onNodeWithText("Aucun compte Claude Code", substring = true).assertExists()
            capture("connections-light")
            compose.onNodeWithContentDescription("Ajouter un compte").performClick()
            waitText("Ajouter un compte")
            compose.onNodeWithText("Compte Claude", substring = true).performClick()
            compose.onNodeWithText("Nom").performTextInput("Studio")
            compose.onNodeWithText("Continuer vers la connexion").performClick()
            waitText("Collez le code affiché par Anthropic")
            capture("claude-sign-in-light")
            compose.onNodeWithText("Code d’autorisation Claude").performTextInput("fixture-code")
            compose.onNodeWithText("Terminer la connexion").performScrollTo().performClick()
            compose.waitUntil(15000) {
                server.requests.any {
                    it.first == "POST /api/installations/fixture/api/accounts/sign-in/code"
                }
            }
            val added =
                wireJson
                    .parseToJsonElement(
                        server.requests
                            .first { it.first == "POST /api/installations/fixture/api/accounts" }
                            .second
                    )
                    .jsonObject
            assertEquals("claude", added["provider"]?.jsonPrimitive?.content)
            assertEquals("Studio", added["name"]?.jsonPrimitive?.content)
            assertEquals(
                """{"code":"fixture-code"}""",
                server.requests
                    .first {
                        it.first == "POST /api/installations/fixture/api/accounts/sign-in/code"
                    }
                    .second,
            )
            compose.waitUntil(15000) {
                compose
                    .onAllNodesWithText("Collez le code affiché par Anthropic")
                    .fetchSemanticsNodes()
                    .isEmpty()
            }
            waitText("studio@example.test")
            compose.onNodeWithText("Studio", substring = true).assertExists()
        }
    }

    @Test
    fun `the detail sheet changes parallel runs and pauses the account`() {
        val server = Accounts().apply { accounts += work }
        journey(
            server,
            { request, payload ->
                if (request.method == "PATCH") {
                    assertEquals("/api/installations/fixture/api/accounts/work", request.path)
                    val update = wireJson.parseToJsonElement(payload).jsonObject
                    update["maxConcurrentRuns"]?.let {
                        server.accounts[0] =
                            server.accounts[0].replace(
                                "\"maxConcurrentRuns\":4",
                                "\"maxConcurrentRuns\":${it.jsonPrimitive.int}",
                            )
                    }
                    update["enabled"]?.let {
                        server.accounts[0] =
                            server.accounts[0].replace(
                                "\"status\":\"next\"",
                                "\"status\":\"paused\",\"enabled\":false",
                            )
                    }
                    "{}"
                } else null
            },
        ) {
            waitText("Work")
            compose.onNodeWithText("1 en cours").assertExists()
            compose.onNodeWithText("Work", substring = true).performClick()
            waitText("USAGE RESTANT")
            compose.onNodeWithText("3 réinitialisations en réserve").assertExists()
            compose.onNodeWithText("5 heures · 96 %").assertExists()
            compose.onNodeWithText("Semaine · 82 %").assertExists()
            waitText("Revue des dépendances")
            capture("account-sheet-light")
            compose
                .onNodeWithContentDescription("Plus d’exécutions parallèles")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                server.requests.any {
                    it.first == "PATCH /api/installations/fixture/api/accounts/work"
                }
            }
            assertEquals(
                """{"maxConcurrentRuns":5}""",
                server.requests
                    .first { it.first == "PATCH /api/installations/fixture/api/accounts/work" }
                    .second,
            )
            waitText("EN COURS · 1 SUR 5")
            compose
                .onNodeWithText("Utiliser pour les nouvelles exécutions")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                server.requests.count {
                    it.first == "PATCH /api/installations/fixture/api/accounts/work"
                } == 2
            }
            assertEquals(
                """{"enabled":false}""",
                server.requests
                    .last { it.first == "PATCH /api/installations/fixture/api/accounts/work" }
                    .second,
            )
            waitText("En pause")
        }
    }

    @Test
    fun `old agents default to Codex and the Claude provider survives serialization`() {
        val old = wireJson.decodeFromString<Agent>("""{"name":"Existing"}""")
        assertEquals("codex", old.provider)
        val claude = old.copy(provider = "claude", model = "sonnet", reasoning = "high")
        assertEquals(claude, wireJson.decodeFromString<Agent>(wireJson.encodeToString(claude)))
    }

    @Test
    fun `a Codex sign-in in progress reopens with its code and can be cancelled`() {
        val server =
            Accounts().apply {
                accounts += work
                signIn =
                    """{"accountId":"work","provider":"codex","state":"pending","phase":"authorizing","code":"ABCD-12345","url":"https://auth.openai.com/codex/device"}"""
            }
        journey(
            server,
            { request, _ ->
                if (
                    request.method == "DELETE" &&
                        request.path == "/api/installations/fixture/api/accounts/sign-in"
                ) {
                    server.signIn = "null"
                    """{"cancelled":true}"""
                } else null
            },
        ) {
            waitText("Saisissez ce code sur OpenAI")
            compose.onNodeWithContentDescription("Code de vérification ABCD-12345").assertExists()
            compose.onNodeWithText("Ouvrir la page de vérification").assertExists()
            capture("codex-sign-in-light")
            compose.onNodeWithText("Annuler la connexion").performScrollTo().performClick()
            compose.waitUntil(10000) {
                server.requests.any {
                    it.first == "DELETE /api/installations/fixture/api/accounts/sign-in"
                }
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Saisissez ce code sur OpenAI")
                    .fetchSemanticsNodes()
                    .isEmpty()
            }
        }
    }
}
