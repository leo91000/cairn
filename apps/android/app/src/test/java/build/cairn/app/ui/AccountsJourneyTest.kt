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
import java.util.UUID
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

private const val WORK_ACCOUNT_ID = "00000000-0000-4000-8000-000000000001"
private const val WORK_REVISION = "00000000-0000-4000-8000-000000000002"

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
        """{"id":"$WORK_ACCOUNT_ID","revision":"$WORK_REVISION","provider":"codex","name":"Work","email":"work@example.test","plan":"plus","state":"ready","status":"next","stale":false,"remainingPercent":82.0,"activeRunIds":["run"],"maxConcurrentRuns":4,"usage":{"checkedAt":1,"windows":[{"id":"main:primary","label":"5-hour window","usedPercent":4,"durationMins":300,"resetsAt":1893499200},{"id":"main:secondary","label":"Weekly","usedPercent":18,"durationMins":10080,"resetsAt":1894017600}],"resets":{"available":3}}}"""

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
                                    "/api/installations" -> beaconInstallationsFixture()
                                    "/api/account/session" -> beaconAccountFixture("fixture")
                                    "/api/installations/fixture/api/accounts" ->
                                        """{"accounts":[${server.accounts.joinToString(",")}],"signIn":${server.signIn}}"""
                                    "/api/installations/fixture/api/runs" ->
                                        """[{"id":"run","status":"running","taskName":"Revue des dépendances","taskId":"fixture-task"}]"""
                                    "/api/installations/fixture/api/overview",
                                    "/api/installations/fixture/api/codex/models",
                                    "/api/installations/fixture/api/claude/models" -> "{}"
                                    else ->
                                        if (
                                            request.requestUrl
                                                ?.encodedPath
                                                ?.endsWith("/api/projects") == true
                                        )
                                            """{"projects":[]}"""
                                        else if (
                                            request.requestUrl
                                                ?.encodedPath
                                                ?.endsWith("/api/agents") == true
                                        )
                                            """{"agents":[]}"""
                                        else if (
                                            request.requestUrl
                                                ?.encodedPath
                                                ?.endsWith("/api/tasks") == true
                                        )
                                            """{"tasks":[]}"""
                                        else if (
                                            request.requestUrl
                                                ?.encodedPath
                                                ?.endsWith("/api/mcps") == true
                                        )
                                            """{"mcps":[]}"""
                                        else "[]"
                                }
                        return MockResponse().setBody(result)
                    }
                }
            mock.start()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MemoryVault(),
                    beaconOrigin = "",
                )
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(mock.url("/").toString())
                }
                CairnTheme("light") {
                    if (state.session.authenticated) ConnectionsScreen(vm, state)
                }
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
                        """{"kind":"signIn","signIn":${server.signIn}}"""
                    }
                    "POST /api/installations/fixture/api/accounts/sign-in/code" -> {
                        server.signIn =
                            """{"accountId":"studio","provider":"claude","state":"complete","phase":"verifying"}"""
                        server.accounts[1] =
                            """{"id":"studio","provider":"claude","name":"Studio","email":"studio@example.test","plan":"max","state":"ready","status":"next","stale":false,"remainingPercent":40.0,"usage":{"checkedAt":1,"windows":[{"id":"five_hour","label":"5-hour window","usedPercent":25,"durationMins":300},{"id":"seven_day","label":"Weekly","usedPercent":60,"durationMins":10080}]}}"""
                        """{"kind":"completed","done":true}"""
                    }
                    else -> null
                }
            },
        ) {
            waitText("AGENTS DE CODE")
            waitText("Work")
            compose.onNodeWithText("Aucun compte Claude Code", substring = true).assertExists()
            // GitHub accounts are plugin instances now: Connections lists coding agents only.
            compose.onNodeWithText("GitHub").assertDoesNotExist()
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
                if (request.method == "PUT") {
                    assertEquals(
                        "/api/installations/fixture/api/accounts/$WORK_ACCOUNT_ID",
                        request.path,
                    )
                    val command = wireJson.parseToJsonElement(payload).jsonObject
                    val current = wireJson.parseToJsonElement(server.accounts[0]).jsonObject
                    assertEquals(current["revision"], command["revision"])
                    val update = command.getValue("settings").jsonObject
                    update["maxConcurrentRuns"]?.let {
                        server.accounts[0] =
                            server.accounts[0].replace(
                                "\"maxConcurrentRuns\":4",
                                "\"maxConcurrentRuns\":${it.jsonPrimitive.int}",
                            )
                    }
                    update["enabled"]
                        ?.takeIf { !it.jsonPrimitive.boolean }
                        ?.let {
                            server.accounts[0] =
                                server.accounts[0].replace(
                                    "\"status\":\"next\"",
                                    "\"status\":\"paused\",\"enabled\":false",
                                )
                        }
                    val updated = wireJson.parseToJsonElement(server.accounts[0]).jsonObject
                    server.accounts[0] =
                        JsonObject(
                                updated +
                                    ("revision" to JsonPrimitive(UUID.randomUUID().toString()))
                            )
                            .toString()
                    server.accounts[0]
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
                    it.first == "PUT /api/installations/fixture/api/accounts/$WORK_ACCOUNT_ID"
                }
            }
            assertEquals(
                5,
                wireJson
                    .parseToJsonElement(
                        server.requests
                            .first {
                                it.first ==
                                    "PUT /api/installations/fixture/api/accounts/$WORK_ACCOUNT_ID"
                            }
                            .second
                    )
                    .jsonObject
                    .getValue("settings")
                    .jsonObject
                    .getValue("maxConcurrentRuns")
                    .jsonPrimitive
                    .int,
            )
            waitText("EN COURS · 1 SUR 5")
            compose
                .onNodeWithText("Utiliser pour les nouvelles exécutions")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                server.requests.count {
                    it.first == "PUT /api/installations/fixture/api/accounts/$WORK_ACCOUNT_ID"
                } == 2
            }
            assertEquals(
                false,
                wireJson
                    .parseToJsonElement(
                        server.requests
                            .last {
                                it.first ==
                                    "PUT /api/installations/fixture/api/accounts/$WORK_ACCOUNT_ID"
                            }
                            .second
                    )
                    .jsonObject
                    .getValue("settings")
                    .jsonObject
                    .getValue("enabled")
                    .jsonPrimitive
                    .boolean,
            )
            waitText("En pause")
        }
    }

    @Test
    fun `the generated Claude profile survives serialization`() {
        val claude = newAgent().copy(provider = "claude", model = "sonnet", reasoning = "high")
        assertEquals(claude, wireJson.decodeFromString<Agent>(wireJson.encodeToString(claude)))
    }

    @Test
    fun `a Codex sign-in in progress reopens with its code and can be cancelled`() {
        val server =
            Accounts().apply {
                accounts += work
                signIn =
                    """{"accountId":"$WORK_ACCOUNT_ID","provider":"codex","state":"pending","phase":"authorizing","code":"ABCD-12345","url":"https://auth.openai.com/codex/device"}"""
            }
        journey(
            server,
            { request, _ ->
                if (
                    request.method == "DELETE" &&
                        request.path == "/api/installations/fixture/api/accounts/sign-in"
                ) {
                    server.signIn = "null"
                    """{"kind":"completed","done":true}"""
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

    @Test
    fun `OpenCode saves a model-provider key through the shared account sheet`() {
        val server = Accounts()
        journey(
            server,
            { request, _ ->
                when ("${request.method} ${request.path}") {
                    "GET /api/installations/fixture/api/accounts/providers/opencode" ->
                        """{"kind":"available","providers":[{"id":"opencode","name":"OpenCode Zen"},{"id":"openai","name":"OpenAI"}]}"""
                    "POST /api/installations/fixture/api/accounts" -> {
                        assertEquals("fixture", request.getHeader("X-CSRF-Token"))
                        val account =
                            """{"id":"zen","provider":"opencode","name":"Zen","modelProvider":"opencode","modelProviderName":"OpenCode Zen","state":"ready","status":"next"}"""
                        server.accounts += account
                        """{"kind":"connected","account":$account}"""
                    }
                    else -> null
                }
            },
        ) {
            waitText("AGENTS DE CODE")
            compose.waitUntil(15000) {
                compose
                    .onAllNodes(hasContentDescription("Ajouter un compte") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithContentDescription("Ajouter un compte").performClick()
            waitText("Ajouter un compte")
            compose
                .onNode(
                    hasText("OpenCode") and
                        hasClickAction() and
                        hasAnyAncestor(hasTestTag("account-sign-in"))
                )
                .performClick()
            waitText("Une clé API par fournisseur de modèles.")
            waitText("OpenCode Zen")
            compose.onNodeWithText("Nom").performTextInput("Zen")
            capture("opencode-connect-light")
            compose
                .onNode(hasText("Clé API") and hasSetTextAction())
                .performTextInput("synthetic-android-zen-key")
            compose.onNodeWithText("Enregistrer la clé API").performScrollTo().performClick()
            compose.waitUntil(15000) {
                server.requests.any { it.first == "POST /api/installations/fixture/api/accounts" }
            }
            val saved =
                wireJson
                    .parseToJsonElement(
                        server.requests
                            .first { it.first == "POST /api/installations/fixture/api/accounts" }
                            .second
                    )
                    .jsonObject
            assertEquals("opencode", saved["provider"]?.jsonPrimitive?.content)
            assertEquals("opencode", saved["modelProvider"]?.jsonPrimitive?.content)
            assertEquals("synthetic-android-zen-key", saved["apiKey"]?.jsonPrimitive?.content)
            compose.waitUntil(15000) {
                compose.onAllNodesWithTag("account-sign-in").fetchSemanticsNodes().isEmpty()
            }
            waitText("Zen")
            waitText("Usage non consulté")
            compose.onNodeWithText("Usage non consulté", substring = true).assertExists()
            capture("opencode-account-light")
        }
    }

    private fun openAntigravity() {
        waitText("AGENTS DE CODE")
        compose.waitUntil(15000) {
            compose
                .onAllNodes(hasContentDescription("Ajouter un compte") and isEnabled())
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
        compose.onNodeWithContentDescription("Ajouter un compte").performClick()
        waitText("Ajouter un compte")
        compose
            .onNode(
                hasText("Antigravity") and
                    hasClickAction() and
                    hasAnyAncestor(hasTestTag("account-sign-in"))
            )
            .performClick()
        waitText("Une clé API Gemini, des identifiants Google Cloud ou un compte Google.")
    }

    private fun savedAccount(server: Accounts) =
        wireJson
            .parseToJsonElement(
                server.requests
                    .first { it.first == "POST /api/installations/fixture/api/accounts" }
                    .second
            )
            .jsonObject

    @Test
    fun `Antigravity saves a Gemini API key and never checks its usage`() {
        val server = Accounts()
        journey(
            server,
            { request, _ ->
                when ("${request.method} ${request.path}") {
                    "POST /api/installations/fixture/api/accounts" -> {
                        val account =
                            """{"id":"gemini","provider":"antigravity","name":"Gemini","modelProvider":"gemini","modelProviderName":"Gemini API","state":"ready","status":"next"}"""
                        server.accounts += account
                        """{"kind":"connected","account":$account}"""
                    }
                    else -> null
                }
            },
        ) {
            openAntigravity()
            compose.onNodeWithText("Nom").performTextInput("Gemini")
            capture("antigravity-connect-light")
            compose
                .onNode(hasText("Clé API Gemini") and hasSetTextAction())
                .performTextInput("synthetic-android-gemini-key")
            compose.onNodeWithText("Enregistrer la clé API").performScrollTo().performClick()
            compose.waitUntil(15000) {
                server.requests.any { it.first == "POST /api/installations/fixture/api/accounts" }
            }
            val saved = savedAccount(server)
            assertEquals("antigravity", saved["provider"]?.jsonPrimitive?.content)
            assertEquals("gemini", saved["modelProvider"]?.jsonPrimitive?.content)
            assertEquals("synthetic-android-gemini-key", saved["apiKey"]?.jsonPrimitive?.content)
            // The providers endpoint is not needed: Antigravity's connection kinds are fixed.
            assertTrue(server.requests.none { it.first.contains("/providers/") })
            compose.waitUntil(15000) {
                compose.onAllNodesWithTag("account-sign-in").fetchSemanticsNodes().isEmpty()
            }
            waitText("Usage non consulté")
            compose.onNode(hasContentDescription("Gemini, ", substring = true)).performClick()
            waitText("Consultez l’usage de votre projet dans le tableau de bord Google.")
            compose.onNodeWithText("Remplacer la clé API").assertExists()
        }
    }

    @Test
    fun `Antigravity composes Google Cloud credentials, project and region into one key`() {
        val server = Accounts()
        journey(
            server,
            { request, _ ->
                when ("${request.method} ${request.path}") {
                    "POST /api/installations/fixture/api/accounts" -> {
                        val account =
                            """{"id":"cloud","provider":"antigravity","name":"Cloud","modelProvider":"google-cloud","modelProviderName":"Google Cloud","state":"ready","status":"next"}"""
                        server.accounts += account
                        """{"kind":"connected","account":$account}"""
                    }
                    else -> null
                }
            },
        ) {
            openAntigravity()
            compose.onNode(hasText("Google Cloud") and hasClickAction()).performClick()
            waitText("Choisir un fichier JSON")
            compose.onNodeWithText("Nom").performTextInput("Cloud")
            val credentials = compose.onNode(hasText("Identifiants JSON") and hasSetTextAction())
            credentials.performTextInput("{not json")
            waitText("Ce fichier n’est pas un JSON valide.")
            credentials.performTextClearance()
            credentials.performTextInput(
                """{"type":"service_account","client_email":"agent@cairn-test.iam.gserviceaccount.com"}"""
            )
            compose
                .onNode(hasText("Projet") and hasSetTextAction())
                .performScrollTo()
                .performTextInput("cairn-test")
            capture("antigravity-google-cloud-light")
            compose.onNodeWithText("Enregistrer les identifiants").performScrollTo().performClick()
            compose.waitUntil(15000) {
                server.requests.any { it.first == "POST /api/installations/fixture/api/accounts" }
            }
            val saved = savedAccount(server)
            assertEquals("google-cloud", saved["modelProvider"]?.jsonPrimitive?.content)
            val key = wireJson.parseToJsonElement(saved.getValue("apiKey").jsonPrimitive.content)
            assertEquals(
                "service_account",
                key.jsonObject["credentials"]?.jsonObject?.get("type")?.jsonPrimitive?.content,
            )
            assertEquals("cairn-test", key.jsonObject["project"]?.jsonPrimitive?.content)
            // A blank region is left to the server, which defaults to global.
            assertFalse("location" in key.jsonObject)
            compose.waitUntil(15000) {
                compose.onAllNodesWithTag("account-sign-in").fetchSemanticsNodes().isEmpty()
            }
            compose.onNode(hasContentDescription("Cloud, ", substring = true)).performClick()
            waitText("Remplacer les identifiants")
        }
    }

    @Test
    fun `an Antigravity Google account signs in only after the terms warning`() {
        val server = Accounts()
        journey(
            server,
            { request, _ ->
                when ("${request.method} ${request.path}") {
                    "POST /api/installations/fixture/api/accounts" -> {
                        server.signIn =
                            """{"accountId":"00000000-0000-4000-8000-00000000a6c1","provider":"antigravity","state":"pending","phase":"authorizing","url":"https://accounts.google.com/o/oauth2/auth?fixture=1","acceptsCode":true}"""
                        server.accounts +=
                            """{"id":"00000000-0000-4000-8000-00000000a6c1","provider":"antigravity","name":"Perso","state":"pending","status":"signIn"}"""
                        """{"kind":"signIn","signIn":${server.signIn}}"""
                    }
                    "POST /api/installations/fixture/api/accounts/sign-in/code" -> {
                        server.signIn =
                            """{"accountId":"00000000-0000-4000-8000-00000000a6c1","provider":"antigravity","state":"complete","phase":"verifying"}"""
                        server.accounts[0] =
                            """{"id":"00000000-0000-4000-8000-00000000a6c1","provider":"antigravity","name":"Perso","email":"perso@example.test","modelProvider":"google","modelProviderName":"Google account","state":"ready","status":"paused","enabled":false,"antigravityQuota":{"stale":true,"windows":[{"id":"weekly","label":"Weekly · Gemini Models","remainingPercent":75}]}}"""
                        """{"kind":"completed","done":true}"""
                    }
                    "POST /api/installations/fixture/api/accounts/00000000-0000-4000-8000-00000000a6c1/sign-in" -> {
                        server.signIn =
                            """{"accountId":"00000000-0000-4000-8000-00000000a6c1","provider":"antigravity","state":"pending","phase":"authorizing","url":"https://accounts.google.com/o/oauth2/auth?fixture=2","acceptsCode":true}"""
                        """{"kind":"signIn","signIn":${server.signIn}}"""
                    }
                    else -> null
                }
            },
        ) {
            openAntigravity()
            compose.onNode(hasText("Compte Google") and hasClickAction()).performClick()
            compose.onNodeWithText("Nom").performTextInput("Perso")
            waitText("Google peut suspendre le compte")
            capture("antigravity-google-warning-light")
            compose.onNodeWithText("Continuer vers la connexion").performScrollTo().performClick()
            waitText("Se connecter avec un compte Google ?")
            compose.onNodeWithText("Annuler").performClick()
            compose.waitForIdle()
            assertTrue(server.requests.none { it.first.startsWith("POST") })
            compose.onNodeWithText("Continuer vers la connexion").performScrollTo().performClick()
            waitText("Se connecter avec un compte Google ?")
            compose.onNodeWithText("J’accepte le risque, continuer").performClick()
            waitText("Collez le code affiché par Google")
            val added = savedAccount(server)
            assertEquals("antigravity", added["provider"]?.jsonPrimitive?.content)
            assertNull(added["modelProvider"]?.takeUnless { it is JsonNull })
            assertNull(added["apiKey"]?.takeUnless { it is JsonNull })
            compose.onNodeWithText("Ouvrir la connexion Google").assertExists()
            capture("antigravity-google-sign-in-light")
            compose.onNodeWithText("Code d’autorisation Google").performTextInput("google-code")
            compose.onNodeWithText("Terminer la connexion").performScrollTo().performClick()
            compose.waitUntil(15000) {
                server.requests.any {
                    it.first == "POST /api/installations/fixture/api/accounts/sign-in/code"
                }
            }
            waitText("perso@example.test")
            compose.waitUntil(15000) {
                compose.onAllNodesWithTag("account-sign-in").fetchSemanticsNodes().isEmpty()
            }
            // Reconnecting a Google account signs in again, behind the same warning.
            compose.onNode(hasContentDescription("Perso, ", substring = true)).performClick()
            waitText(
                "Ces valeurs datent. Activez « Utiliser pour les nouvelles exécutions » pour vérifier le quota."
            )
            waitText("Reconnecter")
            compose.onNodeWithText("Reconnecter").performScrollTo().performClick()
            waitText("Se connecter avec un compte Google ?")
            compose.onNodeWithText("J’accepte le risque, continuer").performClick()
            compose.waitUntil(15000) {
                server.requests.any {
                    it.first ==
                        "POST /api/installations/fixture/api/accounts/00000000-0000-4000-8000-00000000a6c1/sign-in"
                }
            }
        }
    }
}
