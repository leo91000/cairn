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

    /** The built-in Discord package as its manifest declares it: nothing native left. */
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
                            "socket",
                            "inbound",
                            "conversations.submit",
                            "conversations.control",
                            "http",
                            "storage",
                        ),
                    instances = PluginInstances("Bot", "Bots"),
                    credential = PluginCredential("Bot token", "authorization"),
                    instanceForm = true,
                    instanceView = true,
                    instanceTest = true,
                    settings =
                        listOf(
                            PluginSetting(
                                key = "guildId",
                                title = "Server",
                                type = "select",
                                required = true,
                                dynamic = true,
                                refresh = true,
                            ),
                            PluginSetting(
                                key = "channelId",
                                title = "Channel",
                                type = "select",
                                required = true,
                                dynamic = true,
                                refresh = true,
                            ),
                            PluginSetting(
                                key = "messageContent",
                                title = "Read replies without a mention",
                                type = "boolean",
                            ),
                            PluginSetting(key = "guildName", title = "Server name", hidden = true),
                            PluginSetting(
                                key = "channelName",
                                title = "Channel name",
                                hidden = true,
                            ),
                            PluginSetting(key = "botName", title = "Bot name", hidden = true),
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
            status = PluginStatus("connected", "Signed in as Cairn Bot"),
            requests = listOf(PluginRequest(id = "r", userId = "300", name = "Sarah")),
            activity =
                listOf(
                    PluginActivity(
                        id = "a",
                        chatId = "chat",
                        userId = "301",
                        name = "Léa",
                        preview = "Fix the deploy",
                        status = "replied",
                    )
                ),
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
    fun discordBotsAreGenericDelegatingInstances() {
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
            // The package's own words and panel; the delegation is generic.
            compose.onNodeWithText("Signed in as Cairn Bot").assertIsDisplayed()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("#support").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("« Fix the deploy »").assertExists()
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
            compose.onNodeWithText("Helper").performScrollTo().performClick()
            capture("plugin-instance-editor")
            for (person in listOf("300", "301")) {
                compose
                    .onNodeWithText("Identifiant d’une personne")
                    .performScrollTo()
                    .performTextInput(person)
                compose.onNodeWithText("Ajouter").performClick()
            }
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
    fun discordBotIsSetUpThroughItsPackageForm() {
        MockWebServer().use { server ->
            val forms = CopyOnWriteArrayList<JsonObject>()
            val saved = CopyOnWriteArrayList<String>()
            server.dispatcher = dispatcher { path, body ->
                when {
                    path.endsWith("/plugins/discord/form") -> {
                        val request = wireJson.parseToJsonElement(body).jsonObject
                        forms += request
                        discordForm(request)
                    }
                    path.endsWith("/plugins/discord/connections") -> {
                        saved += body
                        "{}"
                    }
                    else -> null
                }
            }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, "discord") {} }
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("Ajouter · Bot").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Ajouter · Bot").performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Create the bot").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Nom").performTextInput("#support on Acme")
            compose.onNodeWithText("Bot token").performTextInput("synthetic_android_token")
            // The bot's card behind the editor also says who it is: wait for the form's own words.
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Bot account · in 1 server")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            // The package checked the typed token; the token goes to the installation only.
            assertTrue(
                forms.toString(),
                forms.any {
                    it["changed"]?.jsonPrimitive?.content == "credential" &&
                        it["token"]?.jsonPrimitive?.content == "synthetic_android_token"
                },
            )
            compose.onNodeWithText("Acme HQ").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("#help").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("The bot can’t read this channel").assertExists()
            compose.onNodeWithText("#help").performScrollTo().performClick()
            // The bot's card behind the editor names the same agent.
            compose.onNode(hasText("Helper") and hasClickAction()).performScrollTo().performClick()
            capture("plugin-discord-setup")
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Enregistrer") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { saved.isNotEmpty() }
            val payload = wireJson.parseToJsonElement(saved.single()).jsonObject
            assertEquals("synthetic_android_token", payload["token"]?.jsonPrimitive?.content)
            assertEquals(
                buildJsonObject {
                    put("guildId", "100")
                    put("channelId", "200")
                    put("messageContent", false)
                    put("guildName", "Acme HQ")
                    put("channelName", "help")
                    put("botName", "Cairn Bot")
                },
                payload["settings"],
            )
            assertEquals("requests", payload["accessMode"]?.jsonPrimitive?.content)
            assertEquals("helper", payload["agentId"]?.jsonPrimitive?.content)
            // The token is only ever sent to the installation, never echoed back in settings.
            assertFalse(payload["settings"].toString().contains("synthetic_android_token"))
        }
    }

    /** The Discord package's `form` handler, as a fixture. */
    private fun discordForm(request: JsonObject): String {
        val settings = request["settings"]?.jsonObject.orEmpty()
        val signedIn = request["token"] != null || request["instance"] != null
        if (!signedIn)
            return buildJsonObject {
                putJsonArray("view") {
                    addJsonObject {
                        put("type", "alert")
                        put("tone", "info")
                        put("title", "Create the bot")
                        put("text", "Paste the token.")
                    }
                }
            }
                .toString()
        val guild = settings["guildId"]?.jsonPrimitive?.content == "100"
        val channel = guild && settings["channelId"]?.jsonPrimitive?.content == "200"
        return buildJsonObject {
            putJsonObject("options") {
                putJsonArray("guildId") {
                    addJsonObject {
                        put("value", "100")
                        put("label", "Acme HQ")
                    }
                }
                if (guild)
                    putJsonArray("channelId") {
                        addJsonObject {
                            put("value", "200")
                            put("label", "#help")
                            put("description", "Can reply")
                        }
                        addJsonObject {
                            put("value", "201")
                            put("label", "#leadership")
                            put("description", "The bot can’t read this channel")
                            put("disabled", true)
                        }
                    }
            }
            putJsonObject("values") {
                put("botName", "Cairn Bot")
                if (guild) put("guildName", "Acme HQ")
                if (channel) put("channelName", "help")
            }
            putJsonArray("view") {
                addJsonObject {
                    put("type", "alert")
                    put("tone", "success")
                    put("title", "Signed in as Cairn Bot")
                    put("text", "Bot account · in 1 server")
                }
            }
        }
            .toString()
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

    @Test
    fun anOwnerResumesSetupAndActivatesItsPreparedResources() {
        val entry =
            wireJson.decodeFromString<PluginEntry>(
                """{
            "id":"workspace-service","active":"1","manifest":{
                "id":"workspace-service","name":"Workspace service","version":"1",
                "capabilities":["resources"],"instanceForm":true,
                "settings":[
                    {"key":"clientId","title":"Client ID","required":true,"refresh":true},
                    {"key":"apiKey","title":"App key","type":"secret","required":true},
                    {"key":"setupIdentity","title":"Setup identity","hidden":true}
                ],
                "resources":[{"name":"workspace","title":"Workspaces","singular":"Workspace","settings":[
                    {"key":"externalId","title":"Workspace ID","required":true},
                    {"key":"token","title":"Workspace token","type":"secret","required":true}
                ]}]
            }
        }"""
            )
        val draft = java.util.concurrent.atomic.AtomicReference<JsonObject?>(null)
        val resources = CopyOnWriteArrayList<JsonObject>()
        val writes = CopyOnWriteArrayList<Pair<String, JsonObject>>()
        val active = java.util.concurrent.atomic.AtomicReference<PluginConnection?>(null)
        val root = "/plugins/workspace-service/setup-drafts"
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("/plugins") ->
                                wireJson.encodeToString(
                                    PluginCatalog(
                                        packages = listOf(entry),
                                        connections = listOfNotNull(active.get()),
                                    )
                                )
                            path.endsWith(root) -> buildJsonObject {
                                    put("items", JsonArray(listOfNotNull(draft.get())))
                                }
                                    .toString()
                            path.endsWith("/resources/workspace") ->
                                buildJsonObject { put("items", JsonArray(resources.toList())) }
                                    .toString()
                            else -> null
                        }
                    }
                ) { path, body ->
                    val input = wireJson.parseToJsonElement(body).jsonObject
                    writes.add(path to input)
                    when {
                        path.endsWith("$root/setup1/form") ->
                            """{"options":{},"values":{"setupIdentity":"setup1"},"errors":{},"view":[{"type":"alert","tone":"info","title":"Prepare the service","text":"Save settings before adding workspaces."}]}"""
                        path.endsWith(root) || path.endsWith("$root/setup1") -> {
                            val value = buildJsonObject {
                                put("id", "setup1")
                                put("pluginId", "workspace-service")
                                put("status", "setup")
                                put("name", input["name"] ?: JsonPrimitive("Workspace service"))
                                put("revision", if (draft.get() == null) "rev1" else "rev2")
                                put("settings", input["settings"] ?: JsonObject(emptyMap()))
                                put(
                                    "secretKeys",
                                    if (input["secrets"]?.jsonObject?.isNotEmpty() == true)
                                        JsonArray(listOf(JsonPrimitive("apiKey")))
                                    else JsonArray(emptyList()),
                                )
                                put("createdAt", System.currentTimeMillis())
                                put("expiresAt", System.currentTimeMillis() + 86400000)
                            }
                            draft.set(value)
                            value.toString()
                        }
                        path.endsWith("$root/setup1/resources/workspace") -> {
                            val value = buildJsonObject {
                                put("id", "workspace1")
                                put("connectionId", "setup1")
                                put("collection", "workspace")
                                put("name", input["name"]!!)
                                put("settings", input["settings"]!!)
                                put("revision", "resource1")
                                put("enabled", true)
                                put("secretKeys", JsonArray(listOf(JsonPrimitive("token"))))
                            }
                            resources.add(value)
                            value.toString()
                        }
                        path.endsWith("$root/setup1/activate") -> {
                            active.set(
                                PluginConnection(
                                    id = "setup1",
                                    pluginId = "workspace-service",
                                    name = "Production service",
                                    settings = draft.get()!!["settings"]!!.jsonObject,
                                    status = PluginStatus("ready"),
                                )
                            )
                            draft.set(null)
                            wireJson.encodeToString(active.get()!!)
                        }
                        path.endsWith("/connections/setup1/resources/workspace/workspace1") -> {
                            if (input["name"] != null) {
                                val updated =
                                    JsonObject(
                                        resources.single() +
                                            mapOf(
                                                "name" to input["name"]!!,
                                                "revision" to JsonPrimitive("resource2"),
                                            )
                                    )
                                resources.clear()
                                resources.add(updated)
                                updated.toString()
                            } else {
                                resources.clear()
                                "{}"
                            }
                        }
                        else -> null
                    }
                }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, "workspace-service") {} }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Ajouter une instance")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Ajouter une instance").performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Configurer Workspace service")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Nom") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Nom").performTextReplacement("Production service")
            compose.onNodeWithText("App key").performTextInput("app-android-fixture")
            compose.onNodeWithText("Client ID").performTextInput("client-android-fixture")
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Enregistrer la configuration") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            assertEquals(
                "clientId",
                writes
                    .last { it.first.endsWith("/form") }
                    .second["changed"]
                    ?.jsonPrimitive
                    ?.content,
            )
            compose.onNodeWithText("Enregistrer la configuration").performScrollTo().performClick()
            compose.waitUntil(10000) {
                draft.get()?.get("name")?.jsonPrimitive?.content == "Production service" &&
                    draft.get()?.get("revision")?.jsonPrimitive?.content == "rev2"
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasContentDescription("Fermer") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty() &&
                    compose
                        .onAllNodesWithText("Tout est enregistré")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            compose.onNodeWithContentDescription("Fermer").performClick()
            compose.onNodeWithText("Continuer Production service").performScrollTo().performClick()
            compose
                .onNodeWithText("App key")
                .assert(
                    SemanticsMatcher.expectValue(
                        SemanticsProperties.EditableText,
                        androidx.compose.ui.text.AnnotatedString(""),
                    )
                )
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Ajouter · Workspace") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Ajouter · Workspace").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Nom · Workspace") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Nom · Workspace").performTextInput("Engineering")
            compose.onNodeWithText("Workspace ID").performTextInput("T111")
            compose.onNodeWithText("Workspace token").performTextInput("workspace-android-fixture")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { resources.isNotEmpty() }
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Nouveau · Workspace").fetchSemanticsNodes().isEmpty() &&
                    compose
                        .onAllNodes(hasText("Activer l’instance") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            capture("plugin-setup-prepared-resources")
            compose.onNodeWithText("Activer l’instance").performScrollTo().performClick()
            compose.waitUntil(10000) { active.get() != null }
            val activation = writes.single { it.first.endsWith("/activate") }.second
            assertEquals("rev2", activation["revision"]?.jsonPrimitive?.content)
            assertEquals(JsonArray(emptyList()), activation["agentIds"])
            val saved = writes.single { it.first.endsWith("$root/setup1") }.second
            assertEquals(
                "app-android-fixture",
                saved["secrets"]?.jsonObject?.get("apiKey")?.jsonPrimitive?.content,
            )
            assertEquals(
                "setup1",
                saved["settings"]?.jsonObject?.get("setupIdentity")?.jsonPrimitive?.content,
            )
            assertNull(draft.get())
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Modifier Engineering") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Modifier Engineering").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Nom · Workspace") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose
                .onNodeWithText("Workspace token")
                .assert(
                    SemanticsMatcher.expectValue(
                        SemanticsProperties.EditableText,
                        androidx.compose.ui.text.AnnotatedString(""),
                    )
                )
            compose.onNodeWithText("Nom · Workspace").performTextReplacement("Engineering team")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Supprimer Engineering team") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            val resourceUpdate =
                writes
                    .single {
                        it.first.endsWith("/connections/setup1/resources/workspace/workspace1")
                    }
                    .second
            assertEquals(JsonObject(emptyMap()), resourceUpdate["secrets"])
            assertEquals("resource1", resourceUpdate["revision"]?.jsonPrimitive?.content)
            compose.onNodeWithText("Supprimer Engineering team").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Supprimer Engineering team ?")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(10000) { resources.isEmpty() }
            val resourceDelete =
                writes
                    .last {
                        it.first.endsWith("/connections/setup1/resources/workspace/workspace1")
                    }
                    .second
            assertEquals("resource2", resourceDelete["revision"]?.jsonPrimitive?.content)
        }
    }

    @Test
    fun obsoleteSetupsRemainVisibleAfterApprovingASimpleManifest() {
        val entry =
            PluginEntry(
                id = "workspace-service",
                active = "2",
                manifest =
                    PluginManifest(
                        id = "workspace-service",
                        name = "Workspace service",
                        version = "2",
                    ),
            )
        val draft =
            java.util.concurrent.atomic.AtomicReference<PluginSetupDraft?>(
                PluginSetupDraft(
                    id = "old-setup",
                    pluginId = "workspace-service",
                    name = "Old setup",
                    status = "review_needed",
                    revision = "old-revision",
                    expiresAt = System.currentTimeMillis() + 86400000,
                )
            )
        val deleted = CopyOnWriteArrayList<String>()
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("/plugins") ->
                                wireJson.encodeToString(PluginCatalog(packages = listOf(entry)))
                            path.endsWith("/setup-drafts") ->
                                wireJson.encodeToString(PluginPage(listOfNotNull(draft.get())))
                            else -> null
                        }
                    }
                ) { path, body ->
                    if (path.endsWith("/setup-drafts/old-setup")) {
                        deleted.add(body)
                        draft.set(null)
                        "{}"
                    } else null
                }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, "workspace-service") {} }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Abandonner Old setup")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Le package doit être approuvé.").assertExists()
            compose.onNodeWithText("Abandonner Old setup").performScrollTo().performClick()
            compose.onNodeWithText("Annuler").performClick()
            assertEquals(0, deleted.size)
            compose.onNodeWithText("Abandonner Old setup").performScrollTo().performClick()
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(10000) { deleted.isNotEmpty() }
            assertEquals(
                "old-revision",
                wireJson
                    .parseToJsonElement(deleted.single())
                    .jsonObject["revision"]
                    ?.jsonPrimitive
                    ?.content,
            )
        }
    }

    @Test
    fun setupExplainsCredentialErrorsAndAcceptsAnEmptyFormAnswer() {
        val entry =
            PluginEntry(
                id = "setup-feedback",
                active = "1",
                manifest =
                    PluginManifest(
                        id = "setup-feedback",
                        name = "Optional form",
                        version = "1",
                        instanceForm = true,
                        credential =
                            PluginCredential(
                                label = "Service credential",
                                header = "authorization",
                            ),
                        resources =
                            listOf(
                                PluginResourceCollection(
                                    name = "workspace",
                                    title = "Workspaces",
                                    singular = "Workspace",
                                )
                            ),
                    ),
            )
        val draft =
            java.util.concurrent.atomic.AtomicReference(
                PluginSetupDraft(
                    id = "setup-feedback1",
                    pluginId = "setup-feedback",
                    name = "Optional form",
                    revision = "revision1",
                    expiresAt = System.currentTimeMillis() + 86400000,
                )
            )
        val activated = java.util.concurrent.atomic.AtomicBoolean(false)
        val formRequests = CopyOnWriteArrayList<JsonObject>()
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("/plugins") ->
                                wireJson.encodeToString(PluginCatalog(packages = listOf(entry)))
                            path.endsWith("/resources/workspace") -> """{"items":[]}"""
                            else -> null
                        }
                    }
                ) { path, body ->
                    val input = wireJson.parseToJsonElement(body).jsonObject
                    when {
                        path.endsWith("/setup-drafts") -> wireJson.encodeToString(draft.get())
                        path.endsWith("/setup-feedback1/form") -> {
                            formRequests.add(input)
                            if (input["token"] != null) "null"
                            else if (draft.get().credential)
                                """{"options":null,"values":null,"errors":null,"view":null}"""
                            else
                                """{"errors":{"credential":"Enter the service credential.","verifiedTeam":"Invite the app to the workspace before continuing."}}"""
                        }
                        path.endsWith("/setup-drafts/setup-feedback1") -> {
                            draft.set(draft.get().copy(credential = true, revision = "revision2"))
                            wireJson.encodeToString(draft.get())
                        }
                        path.endsWith("/setup-feedback1/activate") -> {
                            activated.set(true)
                            "{}"
                        }
                        else -> null
                    }
                }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, "setup-feedback") {} }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Ajouter une instance")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Ajouter une instance").performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Enter the service credential.")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose
                .onNodeWithText("Invite the app to the workspace before continuing.")
                .assertExists()
            compose.onNodeWithText("Activer l’instance").assertIsNotEnabled()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Service credential") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose
                .onNodeWithText("Service credential")
                .performTextInput("credential-android-fixture")
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Enregistrer la configuration") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            assertEquals("credential", formRequests.last()["changed"]?.jsonPrimitive?.content)
            compose.onNodeWithText("Enregistrer la configuration").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Activer l’instance") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Activer l’instance").performScrollTo().performClick()
            compose.waitUntil(10000) { activated.get() }
        }
    }

    /** Answers the workspace fixture; custom reads and writes remain HTTP boundaries. */
    private fun dispatcher(
        read: (String) -> String? = { null },
        handle: (String, String) -> String?,
    ) =
        object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse {
                val path = request.path.orEmpty()
                val body = request.body.readUtf8()
                if (request.method == "GET")
                    read(path)?.let {
                        return MockResponse().setBody(it)
                    }
                if (request.method == "GET" && path.endsWith("/setup-drafts"))
                    return MockResponse().setBody("""{"items":[]}""")
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
                        "/api/installations/fixture/api/plugins/discord/connections/bot/view" ->
                            """{"view":[{"type":"facts","items":[{"label":"Channel","value":"#support"},{"label":"Server","value":"Acme"}]}]}"""
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
