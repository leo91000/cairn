package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.*
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

/** Plugin management journeys, run under Robolectric and on a device with screenshots. */
abstract class PluginsCases {
    @get:Rule val compose = createComposeRule()

    protected open fun capture(name: String) {}

    private val discord =
        PluginEntry(
            id = "discord",
            builtin = true,
            active = "1.0.0",
            manifest =
                PluginManifest(
                    id = "discord",
                    name = "Discord",
                    version = "1.0.0",
                    capabilities =
                        listOf(
                            "transport.discord",
                            "conversations.submit",
                            "conversations.control",
                        ),
                ),
        )
    private val notes =
        PluginEntry(
            id = "notes",
            active = "1.0.0",
            manifest =
                PluginManifest(
                    id = "notes",
                    name = "Notes",
                    version = "1.0.0",
                    capabilities = listOf("conversations.submit", "storage"),
                    settings =
                        listOf(
                            PluginSetting(key = "team", title = "Team", required = true),
                            PluginSetting(key = "limit", title = "Limit", type = "number"),
                            PluginSetting(key = "loud", title = "Loud", type = "boolean"),
                            PluginSetting(key = "secret", title = "Secret", hidden = true),
                        ),
                ),
        )
    /** The built-in 1Password package as API v2 declares it, trimmed to what screens read. */
    private val onePassword =
        PluginEntry(
            id = "onepassword",
            builtin = true,
            active = "1.0.0",
            manifest =
                PluginManifest(
                    id = "onepassword",
                    name = "1Password",
                    version = "1.0.0",
                    capabilities = listOf("exec"),
                    instances = PluginInstances("Service account", "Service accounts"),
                    programs = listOf(PluginProgram("op", "/usr/local/bin/op")),
                    settings =
                        listOf(
                            PluginSetting(
                                key = "token",
                                title = "Service account token",
                                type = "secret",
                                required = true,
                            )
                        ),
                    agentAccess = PluginAgentAccess("Off by default."),
                    commands =
                        listOf(
                            PluginCommand(
                                name = "read",
                                output = "secret",
                                arguments =
                                    listOf(
                                        PluginArgument(
                                            "account",
                                            flag = "--account",
                                            required = true,
                                        ),
                                        PluginArgument(
                                            "reference",
                                            positional = true,
                                            required = true,
                                        ),
                                    ),
                            )
                        ),
                    tools = listOf(PluginTool("onepassword", "Metadata")),
                    instanceView = true,
                    instanceTest = true,
                ),
        )
    private val account =
        PluginConnection(
            id = "op1",
            pluginId = "onepassword",
            name = "Production",
            agentIds = listOf(MAIN_AGENT_ID),
            secretKeys = listOf("token"),
            status = PluginStatus("ready"),
        )
    private val bot =
        PluginConnection(
            id = "bot",
            pluginId = "discord",
            name = "Support",
            settings =
                buildJsonObject {
                    put("channelId", "200")
                    put("channelName", "support")
                    put("botName", "Cairn Bot")
                },
            accessMode = "requests",
            agentId = "helper",
            status = PluginStatus("connected"),
            requests = listOf(PluginRequest(id = "r", userId = "300", name = "Sarah")),
        )

    @Test
    fun catalogueSummarisesPluginsAsTheWebDoes() {
        val catalog = listOf(bot, bot.copy(id = "paused", enabled = false))
        assertEquals(
            PluginSummary(PluginBadge("1 en ligne", PluginTone.OK), "2 bots", 2),
            discord.summary(catalog),
        )
        // Instances without a worker are ready; agent grants are counted once.
        assertEquals(
            PluginSummary(
                PluginBadge("1 prêt", PluginTone.OK),
                "1 service account · 1 agent autorisé",
                0,
            ),
            onePassword.summary(listOf(account)),
        )
        assertEquals(
            PluginSummary(PluginBadge("À configurer", PluginTone.MUTED), "Version 1.0.0", 0),
            onePassword.summary(emptyList()),
        )
        assertEquals(
            PluginBadge("À approuver", PluginTone.WARNING),
            notes.copy(status = "needs_approval").summary(emptyList()).badge,
        )
        assertEquals(
            "cairn plugin onepassword read --account <account> <reference>",
            onePassword.manifest!!.commands.single().usage("onepassword"),
        )
        assertEquals(PluginBadge("En pause", PluginTone.MUTED), bot.copy(enabled = false).state())
        assertEquals(
            PluginBadge("À vérifier", PluginTone.ATTENTION),
            bot.copy(status = PluginStatus("needs_review")).state(),
        )
    }

    @Test
    fun formsKeepChoicesAndNeverSendSecretsAsSettings() {
        val fields =
            listOf(
                PluginSetting(key = "token", title = "Token", type = "secret"),
                PluginSetting(
                    key = "region",
                    title = "Region",
                    type = "select",
                    options = listOf(PluginOption("eu", "Europe")),
                ),
                PluginSetting(key = "scopes", title = "Scopes", type = "multiselect"),
            )
        assertEquals(
            buildJsonObject {
                put("region", "eu")
                put("scopes", JsonArray(listOf(JsonPrimitive("read"))))
            },
            pluginSettings(
                fields,
                mapOf("token" to "secret", "region" to "eu", "scopes" to "[\"read\"]"),
            ),
        )
        assertEquals("[]", formValue(fields[2], null))
    }

    @Test
    fun settingsKeepNumbersAndBooleansAndDropBlankText() {
        val fields = notes.manifest!!.settings
        assertEquals(
            buildJsonObject {
                put("team", "core")
                put("limit", 3)
                put("loud", false)
            },
            pluginSettings(
                fields,
                mapOf("team" to "core", "limit" to "3", "loud" to "false", "secret" to ""),
            ),
        )
    }

    @Test
    fun theCatalogueOpens1PasswordAsAPlugin() {
        MockWebServer().use { server ->
            server.dispatcher = dispatcher { _, _ -> null }
            server.start()
            var opened = ""
            show(server) { vm, state -> PluginsScreen(vm, state) { opened = it } }
            compose.waitUntil(30000) {
                compose.onAllNodesWithTag("plugin-onepassword").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("1 demande").assertIsDisplayed()
            capture("plugins-catalogue")
            compose.onNodeWithTag("plugin-onepassword").performClick()
            assertEquals("onepassword", opened)
        }
    }

    @Test
    fun discordRequestsAndPauseAreManagedNatively() {
        MockWebServer().use { server ->
            val actions = CopyOnWriteArrayList<Pair<String, String>>()
            server.dispatcher = dispatcher { path, body ->
                if (path.contains("/connections/bot/")) {
                    actions += path.substringAfter("/connections/bot/") to body
                    "{}"
                } else null
            }
            server.start()
            val vm = show(server) { vm, state -> PluginScreen(vm, state, "discord") {} }
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("Approuver").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("#support").assertIsDisplayed()
            capture("plugin-discord")
            compose.onNodeWithText("Approuver").performScrollTo().performClick()
            compose.waitUntil(10000) { actions.isNotEmpty() }
            assertEquals("people/300/approve", actions.first().first)
            // Querying the tree lets the main looper deliver the action's completion.
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Approuver") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty() && !vm.state.value.busy
            }
            compose
                .onNodeWithText("Accepter les nouveaux messages")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) { actions.size == 2 }
            assertEquals("enabled", actions[1].first)
            assertEquals(
                false,
                wireJson
                    .parseToJsonElement(actions[1].second)
                    .jsonObject["enabled"]
                    ?.jsonPrimitive
                    ?.boolean,
            )
        }
    }

    @Test
    fun genericInstanceIsCreatedWithItsSettingsAndDelegation() {
        MockWebServer().use { server ->
            val saved = CopyOnWriteArrayList<String>()
            server.dispatcher = dispatcher { path, body ->
                if (path.endsWith("/plugins/notes/connections")) {
                    saved += body
                    "{}"
                } else null
            }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, "notes") {} }
            compose.waitUntil(30000) {
                compose
                    .onAllNodesWithText("Ajouter une instance")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Ajouter une instance").performClick()
            compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
            compose.onNodeWithText("Team").performTextInput("core")
            compose.onNodeWithText("Limit").performTextInput("3")
            compose.onNodeWithText("Choisir…").performScrollTo().performClick()
            compose.onNodeWithText("Helper").performClick()
            capture("plugin-instance-editor")
            compose
                .onNodeWithText("Identifiants des personnes")
                .performScrollTo()
                .performTextInput("300, 301")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { saved.isNotEmpty() }
            val payload = wireJson.parseToJsonElement(saved.single()).jsonObject
            assertEquals("Notes", payload["name"]?.jsonPrimitive?.content)
            assertEquals(
                buildJsonObject {
                    put("team", "core")
                    put("limit", 3)
                    put("loud", false)
                },
                payload["settings"],
            )
            assertEquals("helper", payload["agentId"]?.jsonPrimitive?.content)
            assertEquals("project", payload["projectId"]?.jsonPrimitive?.content)
            assertEquals(
                JsonArray(listOf(JsonPrimitive("300"), JsonPrimitive("301"))),
                payload["userIds"],
            )
            assertFalse(payload.containsKey("token"))
        }
    }

    @Test
    fun onePasswordIsAGenericPluginWithItsPanelAndAgentUsage() {
        MockWebServer().use { server ->
            val actions = CopyOnWriteArrayList<String>()
            server.dispatcher = dispatcher { path, body ->
                if (path.endsWith("/connections/op1/actions")) {
                    actions += body
                    """{"view":[{"type":"list","items":[{"title":"Production vault"}]}]}"""
                } else null
            }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, "onepassword") {} }
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("Show vaults").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Enregistré").assertExists()
            compose.onNodeWithText("Agents autorisés : Cairn").assertExists()
            capture("plugin-onepassword")
            compose.onNodeWithText("Show vaults").performScrollTo().performClick()
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("Production vault").fetchSemanticsNodes().isNotEmpty()
            }
            assertEquals(
                "vaults",
                wireJson
                    .parseToJsonElement(actions.single())
                    .jsonObject["action"]
                    ?.jsonPrimitive
                    ?.content,
            )
            compose.onNodeWithTag("plugin-agent-usage").performScrollTo()
            compose
                .onNodeWithText("cairn plugin onepassword read --account <account> <reference>")
                .assertExists()
            compose.onNodeWithText("/usr/local/bin/op").assertExists()
            capture("plugin-onepassword-agents")
        }
    }

    @Test
    fun serviceAccountsSendTheirTokenOnceWithExplicitGrants() {
        MockWebServer().use { server ->
            val saved = CopyOnWriteArrayList<String>()
            server.dispatcher = dispatcher { path, body ->
                if (path.endsWith("/plugins/onepassword/connections")) {
                    saved += body
                    "{}"
                } else null
            }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, "onepassword") {} }
            compose.waitUntil(30000) {
                compose
                    .onAllNodesWithText("Ajouter · Service account")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Ajouter · Service account").performClick()
            compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
            compose.onNodeWithText("Nom").performTextInput("Staging")
            compose.onNodeWithText("Service account token").performTextInput("ops_android_fixture")
            compose
                .onNode(SemanticsMatcher.keyIsDefined(SemanticsProperties.Password))
                .assertExists()
            compose.onNodeWithText("Helper").performScrollTo().performClick()
            capture("plugin-instance-editor")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { saved.isNotEmpty() }
            val payload = wireJson.parseToJsonElement(saved.single()).jsonObject
            assertEquals("Staging", payload["name"]?.jsonPrimitive?.content)
            assertEquals(
                buildJsonObject { put("token", "ops_android_fixture") },
                payload["secrets"],
            )
            assertEquals(JsonArray(listOf(JsonPrimitive("helper"))), payload["agentIds"])
            assertEquals(JsonObject(emptyMap()), payload["settings"])
        }
    }

    /** Answers the workspace fixture; [handle] takes plugin writes first. */
    private fun dispatcher(handle: (String, String) -> String?) =
        object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse {
                val path = request.path.orEmpty()
                val body = request.body.readUtf8()
                if (request.method != "GET") {
                    if (request.getHeader("X-CSRF-Token") != "fixture-csrf")
                        return MockResponse().setResponseCode(403)
                    handle(path, body)?.let {
                        return MockResponse().setBody(it)
                    }
                }
                val result =
                    when (path) {
                        "/api/installations" -> beaconInstallationsFixture()
                        "/api/account/session" -> beaconAccountFixture("fixture-csrf")
                        "/api/installations/fixture/api/agents" ->
                            wireJson.encodeToString(
                                listOf(
                                    Agent(id = MAIN_AGENT_ID, name = "Cairn"),
                                    Agent(
                                        id = "helper",
                                        name = "Helper",
                                        access = AccessPolicy(projects = listOf("project")),
                                    ),
                                )
                            )
                        "/api/installations/fixture/api/plugins" ->
                            wireJson.encodeToString(
                                PluginCatalog(
                                    packages =
                                        listOf(
                                            onePassword,
                                            discord,
                                            notes,
                                        ),
                                    connections = listOf(bot, account),
                                )
                            )
                        "/api/installations/fixture/api/plugins/onepassword/connections/op1/view" ->
                            """{"view":[{"type":"section","title":"Vaults","children":[{"type":"actions","children":[{"type":"button","label":"Show vaults","action":"vaults"}]}]}]}"""
                        "/api/installations/fixture/api/overview",
                        "/api/installations/fixture/api/codex/models" -> "{}"
                        else -> "[]"
                    }
                return MockResponse().setBody(result)
            }
        }

    /** Signs in to the fixture, then shows [content] once the workspace has loaded. */
    private fun show(
        server: MockWebServer,
        content: @Composable (CairnViewModel, Workspace) -> Unit,
    ): CairnViewModel {
        val vm =
            CairnViewModel(
                ApplicationProvider.getApplicationContext<Application>(),
                PluginVault(),
                beaconOrigin = "",
            )
        compose.setContent {
            val state by vm.state.collectAsStateWithLifecycle()
            // Once loaded, the screen stays composed even if a refresh briefly empties the agents.
            var loaded by remember { mutableStateOf(false) }
            if (state.agents.isNotEmpty()) loaded = true
            LaunchedEffect(Unit) {
                vm.state.first { it.ready }
                vm.connect(server.url("/").toString())
            }
            // The app's scaffold keeps screens clear of the system bars; this host does the same.
            CairnTheme {
                Surface(Modifier.fillMaxSize()) {
                    Box(Modifier.systemBarsPadding()) {
                        if (loaded) content(vm, state)
                    }
                }
            }
        }
        return vm
    }
}

private class PluginVault : SessionVault {
    private val values = mutableMapOf<String, String>()

    override fun read(origin: String) = values[origin]

    override fun write(origin: String, cookie: String?) {
        if (cookie == null) values.remove(origin) else values[origin] = cookie
    }
}
