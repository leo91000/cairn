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
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
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
            setup =
                PluginSetup(
                    effort = "token",
                    steps =
                        listOf(
                            SetupStep("credential", "credential", "Bot token"),
                            SetupStep(
                                "settings",
                                "settings",
                                "Settings",
                                settings = listOf("guildId", "channelId", "messageContent"),
                            ),
                            SetupStep("people", "people", "Who can talk to it"),
                        ),
                ),
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
            setup =
                PluginSetup(
                    steps =
                        listOf(
                            SetupStep(
                                "settings",
                                "settings",
                                "Settings",
                                settings = listOf("team", "limit", "loud"),
                            ),
                            SetupStep("people", "people", "Who can talk to it"),
                        )
                ),
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
                    programs = listOf(PluginProgram(name = "op", installer = "programs/op.luau")),
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
            // As the installation resolves it from the manifest's `[setup]`.
            setup =
                PluginSetup(
                    effort = "token",
                    summary = "1Password connects through a service account you create.",
                    steps =
                        listOf(
                            SetupStep(
                                "token",
                                title = "Service account token",
                                settings = listOf("token"),
                                checklist =
                                    listOf(
                                        SetupTask(
                                            "Create a service account",
                                            "Copy its token, which starts with ops_.",
                                            url =
                                                "https://developer.1password.com/docs/service-accounts/get-started/",
                                            link = "Open the guide",
                                        )
                                    ),
                            ),
                            SetupStep("agents", "agents", "Agents"),
                        ),
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
            agentId = "08c10fb1-ed4e-5f3d-9592-dbfcefd1cdd3",
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
        // Until the first instance exists, the card says what connecting takes.
        assertEquals(
            PluginSummary(
                PluginBadge("À configurer", PluginTone.MUTED),
                "Version 1.0.0",
                0,
                effort = "token",
            ),
            onePassword.summary(emptyList()),
        )
        assertEquals("Coller un jeton", effortLabel(onePassword.summary(emptyList()).effort))
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
            // One step at a time: the settings, then who can talk to it.
            compose.onNodeWithText("Étape 1 sur 2", ignoreCase = true).assertExists()
            compose.onNodeWithText("Continuer").assertIsNotEnabled()
            compose.onNodeWithText("Renseignez « Team ».").assertExists()
            compose.onNodeWithContentDescription("Étape 2", substring = true).assertIsNotEnabled()
            compose.onNodeWithText("Team").performTextInput("core")
            compose.onNodeWithText("Limit").performTextInput("3")
            compose.onNodeWithText("Continuer").performClick()
            compose.onNode(isHeading() and hasText("Qui peut lui parler")).assertExists()
            compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
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
            assertEquals(
                "08c10fb1-ed4e-5f3d-9592-dbfcefd1cdd3",
                payload["agentId"]?.jsonPrimitive?.content,
            )
            assertEquals("project", payload["projectId"]?.jsonPrimitive?.content)
            assertEquals(
                JsonArray(listOf(JsonPrimitive("300"), JsonPrimitive("301"))),
                payload["userIds"],
            )
            assertFalse(payload.containsKey("token"))
        }
    }

    @Test
    fun ordinaryFieldsClearStaleErrorsAndPresetsRefreshDependentChoices() {
        val entry =
            notes
                .copy(
                    manifest =
                        notes.manifest!!.copy(
                            capabilities = emptyList(),
                            instanceForm = true,
                            settings =
                                listOf(
                                    PluginSetting(key = "team", title = "Team", required = true),
                                    PluginSetting(
                                        key = "more",
                                        title = "More destinations",
                                        type = "boolean",
                                        refresh = true,
                                    ),
                                    PluginSetting(
                                        key = "extra",
                                        title = "Extra destinations",
                                        type = "boolean",
                                        refresh = true,
                                    ),
                                    PluginSetting(
                                        key = "destination",
                                        title = "Destination",
                                        type = "select",
                                        dynamic = true,
                                    ),
                                ),
                            setup =
                                PluginSetup(
                                    steps =
                                        listOf(
                                            SetupStep(
                                                id = "settings",
                                                title = "Settings",
                                                settings =
                                                    listOf("team", "more", "extra", "destination"),
                                                presets =
                                                    listOf(
                                                        SetupPreset("basic", "Basic"),
                                                        SetupPreset(
                                                            "more",
                                                            "More places",
                                                            values =
                                                                mapOf(
                                                                    "more" to true,
                                                                    "extra" to true,
                                                                ),
                                                        ),
                                                    ),
                                            )
                                        )
                                ),
                        )
                )
                .let { it.copy(setup = it.manifest!!.setup) }
        MockWebServer().use { server ->
            val forms = CopyOnWriteArrayList<JsonObject>()
            val saved = CopyOnWriteArrayList<String>()
            server.dispatcher =
                dispatcher(packages = listOf(entry), connections = emptyList()) { path, body ->
                    when {
                        path.endsWith("/plugins/notes/form") -> {
                            val request = wireJson.parseToJsonElement(body).jsonObject
                            forms += request
                            val values = request.getValue("settings").jsonObject
                            wireJson.encodeToString(
                                PluginForm(
                                    errors =
                                        if (values["team"]?.jsonPrimitive?.content.isNullOrEmpty())
                                            mapOf("team" to "Enter a team")
                                        else emptyMap(),
                                    options =
                                        mapOf(
                                            "destination" to
                                                if (
                                                    values["more"] == JsonPrimitive(true) &&
                                                        values["extra"] == JsonPrimitive(true)
                                                )
                                                    listOf(PluginOption("room", "Room"))
                                                else emptyList()
                                        ),
                                )
                            )
                        }
                        path.endsWith("/plugins/notes/connections") -> {
                            saved += body
                            "{}"
                        }
                        else -> null
                    }
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
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Enter a team").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Enter a team").assertExists()
            compose.onNodeWithText("Team").performTextInput("core")
            compose.onNodeWithText("Enregistrer").assertIsEnabled()
            compose.onNodeWithText("Enter a team").assertDoesNotExist()
            compose.onNodeWithText("More places").performScrollTo().performClick()
            compose.waitUntil(10000) { forms.count { it["changed"] != null } == 2 }
            assertEquals(
                listOf("more", "extra"),
                forms.mapNotNull { it["changed"]?.jsonPrimitive?.content },
            )
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Room").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Room").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Enregistrer") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { saved.isNotEmpty() }
            assertEquals(
                JsonPrimitive("room"),
                wireJson
                    .parseToJsonElement(saved.single())
                    .jsonObject["settings"]
                    ?.jsonObject
                    ?.get("destination"),
            )
        }
    }

    @Test
    fun requiredWhitespaceChoiceCanBeSavedWithNonblankUnicodeText() {
        val entry =
            notes.copy(
                setup =
                    PluginSetup(
                        effort = "token",
                        steps =
                            listOf(
                                SetupStep(
                                    "settings",
                                    "settings",
                                    "Settings",
                                    settings = listOf("region", "team", "password"),
                                )
                            ),
                    ),
                manifest =
                    notes.manifest!!.copy(
                        capabilities = emptyList(),
                        settings =
                            listOf(
                                PluginSetting(
                                    key = "region",
                                    title = "Region",
                                    type = "select",
                                    required = true,
                                    default = JsonPrimitive(" "),
                                    options =
                                        listOf(
                                            PluginOption(" ", "Space option"),
                                            PluginOption("eu", "Europe"),
                                        ),
                                ),
                                PluginSetting(key = "team", title = "Team", required = true),
                                PluginSetting(
                                    key = "password",
                                    title = "Password",
                                    type = "secret",
                                    required = true,
                                ),
                            ),
                    ),
            )
        MockWebServer().use { server ->
            val saved = CopyOnWriteArrayList<String>()
            server.dispatcher =
                dispatcher(includeGet = true) { path, body ->
                    when (path) {
                        "/api/installations/fixture/api/plugins" ->
                            wireJson.encodeToString(PluginCatalog(packages = listOf(entry)))
                        "/api/installations/fixture/api/plugins/notes/connections" -> {
                            saved += body
                            "{}"
                        }
                        else -> null
                    }
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
            compose.onNodeWithText("Space option").assertExists()
            compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
            compose.onNodeWithText("Team").performTextInput("   ")
            compose.onNodeWithText("Password").performTextInput(" \uFEFFcredential\uFEFF ")
            compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
            compose.onNodeWithText("Team").performTextReplacement(" \uFEFFteam\uFEFF ")
            // Opening default is a nonempty identifier, even though it is blank as text.
            compose.onNodeWithText("Enregistrer").assertIsEnabled()
            // The same identifier chosen manually must reach the save guard and wire payload.
            compose.onNodeWithText("Space option").performClick()
            compose.onNodeWithText("Europe").performClick()
            compose.onNodeWithText("Europe").performClick()
            compose.onNodeWithText("Space option").performClick()
            compose.onNodeWithText("Enregistrer").assertIsEnabled()
            capture("plugin-whitespace-choice-editor")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { saved.isNotEmpty() }
            val payload = wireJson.parseToJsonElement(saved.single()).jsonObject
            assertEquals(
                buildJsonObject {
                    put("region", " ")
                    put("team", "\uFEFFteam\uFEFF")
                },
                payload["settings"],
            )
            assertEquals(
                buildJsonObject {
                    put("password", "\uFEFFcredential\uFEFF")
                },
                payload["secrets"],
            )
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
            compose
                .onNode(hasSetTextAction() and hasText("Bot token"))
                .performTextInput("synthetic_android_token")
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
            continueSetup()
            compose.onNodeWithText("Acme HQ").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("#help").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("The bot can’t read this channel").assertExists()
            compose.onNodeWithText("#help").performScrollTo().performClick()
            continueSetup()
            // The bot's card behind the editor names the same agent.
            compose.onNode(hasText("Helper") and hasClickAction()).performScrollTo().performClick()
            compose.onNodeWithText("Nom").performScrollTo().performTextInput("#support on Acme")
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
            assertEquals(
                "08c10fb1-ed4e-5f3d-9592-dbfcefd1cdd3",
                payload["agentId"]?.jsonPrimitive?.content,
            )
            // The token is only ever sent to the installation, never echoed back in settings.
            assertFalse(payload["settings"].toString().contains("synthetic_android_token"))
        }
    }

    /** A package whose accounts sign in on the service's page, as the GitHub package does. */
    private val forge =
        PluginEntry(
            id = "forge",
            builtin = true,
            active = "1.0.0",
            setup =
                PluginSetup(
                    effort = "signin",
                    steps = listOf(SetupStep("credential", "credential", "Sign in with Forge")),
                ),
            manifest =
                PluginManifest(
                    id = "forge",
                    name = "Forge",
                    version = "1.0.0",
                    capabilities = listOf("http"),
                    instances = PluginInstances("Account", "Accounts"),
                    credential =
                        PluginCredential(
                            "Forge token",
                            "authorization",
                            "Bearer ",
                            deviceFlow =
                                buildJsonObject {
                                    put("authorizationUrl", "https://forge.example/device/code")
                                    put("tokenUrl", "https://forge.example/token")
                                    put("clientId", "fixture")
                                },
                        ),
                    instanceForm = true,
                    settings =
                        listOf(PluginSetting(key = "login", title = "Account", hidden = true)),
                ),
        )

    private fun deviceFlow(id: String, state: String, message: String? = null) =
        wireJson.encodeToString(
            PluginDeviceFlow(
                id,
                "forge",
                state,
                "WXYZ-${id.takeLast(1)}234",
                "https://forge.example/login/device",
                "https://forge.example/login/device?code=WXYZ",
                expiresAt = 1,
                message = message,
            )
        )

    @Test
    fun deviceSignInHelpersSendTheFlowInsteadOfAToken() {
        val pending = wireJson.decodeFromString<PluginDeviceFlow>(deviceFlow("flow-1", "pending"))
        assertEquals("https://forge.example/login/device?code=WXYZ", pending.link)
        assertEquals(
            "https://forge.example/login/device",
            pending.copy(verificationUriComplete = null).link,
        )
        // Pending: a pasted token, or nothing for the saved credential.
        assertEquals("token" to "pasted", credentialInput(" pasted ", pending))
        assertNull(credentialInput("", pending))
        // Completed: the sign-in replaces any paste; the token never reaches the app.
        val completed = pending.copy(state = "completed")
        assertEquals("credentialFlow" to "flow-1", credentialInput("pasted", completed))
        assertNull(signInProblem(pending))
        assertEquals(
            "Cette connexion a expiré. Recommencez.",
            signInProblem(pending.copy(state = "expired")),
        )
        assertEquals("Refused", signInProblem(pending.copy(state = "failed", message = "Refused")))
        // The manifest's device flow is read without depending on its shape.
        val manifest =
            wireJson.decodeFromString<PluginManifest>(
                """{"id":"github","credential":{"label":"GitHub token","header":"authorization","deviceFlow":{"clientId":"x","scopes":["repo"]}},"projectSources":[{"name":"repositories","title":"GitHub repository"}],"skillSources":[{"name":"repository","title":"GitHub repository","fields":[{"key":"repository","title":"Repository","required":true}]}],"conditions":[{"name":"workflow"}],"runCredentials":{"git":[]}}"""
            )
        assertNotNull(manifest.credential?.deviceFlow)
        assertEquals("repositories", manifest.projectSources.single().name)
        assertEquals("repository", manifest.skillSources.single().fields.single().key)
        val oauth =
            wireJson.decodeFromString<PluginManifest>(
                """{"id":"gmail","credential":{"label":"Google authorization","header":"authorization","oauth":{"clientIdSetting":"clientId","requireRefreshToken":true}}}"""
            )
        assertNotNull(oauth.credential?.oauth)
        assertTrue(pending.copy(state = "exchanging", userCode = "").pending)
    }

    @Test
    fun oauthAccountsUseTheSameEditorWithoutATokenOrDeviceCode() {
        val oauth =
            forge.copy(
                setup =
                    PluginSetup(
                        effort = "app",
                        steps =
                            listOf(
                                SetupStep(
                                    "settings",
                                    "settings",
                                    "Settings",
                                    settings = listOf("clientId", "clientSecret"),
                                ),
                                SetupStep("credential", "credential", "Sign in with Forge"),
                            ),
                    ),
                manifest =
                    requireNotNull(forge.manifest)
                        .copy(
                            credential =
                                PluginCredential(
                                    "Google authorization",
                                    "authorization",
                                    "Bearer ",
                                    oauth = buildJsonObject { put("clientIdSetting", "clientId") },
                                ),
                            settings =
                                listOf(
                                    PluginSetting("clientId", "OAuth client ID", required = true),
                                    PluginSetting(
                                        "clientSecret",
                                        "OAuth client secret",
                                        type = "secret",
                                        required = true,
                                    ),
                                ),
                        ),
            )
        MockWebServer().use { server ->
            val starts = CopyOnWriteArrayList<JsonObject>()
            val saved = CopyOnWriteArrayList<JsonObject>()
            val flow =
                PluginDeviceFlow(
                    "oauth-one",
                    "forge",
                    "pending",
                    "",
                    "https://accounts.google.com/authorize",
                    expiresAt = 1,
                )
            server.dispatcher =
                dispatcher(includeGet = true, packages = listOf(oauth)) { path, body ->
                    when (path.removePrefix("/api/installations/fixture/api")) {
                        "/plugins/forge/form" -> "{}"
                        "/plugins/forge/oauth" ->
                            if (body.isEmpty()) {
                                """{"callbackUrl":"https://cairn.example/plugins/oauth/callback"}"""
                            } else {
                                starts += wireJson.parseToJsonElement(body).jsonObject
                                wireJson.encodeToString(flow)
                            }
                        "/plugins/forge/oauth/oauth-one" ->
                            wireJson.encodeToString(
                                flow.copy(state = "completed", verificationUri = "")
                            )
                        "/plugins/forge/connections" -> {
                            saved += wireJson.parseToJsonElement(body).jsonObject
                            "{}"
                        }
                        else -> null
                    }
                }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, "forge") {} }
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("Ajouter · Account").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Ajouter · Account").performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("OAuth client ID").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Google authorization").assertDoesNotExist()
            compose
                .onNodeWithText("OAuth client ID")
                .performScrollTo()
                .performTextInput("public-client")
            compose
                .onNodeWithText("OAuth client secret")
                .performScrollTo()
                .performTextInput("synthetic-secret")
            continueSetup()
            compose.onNodeWithText("Nom").performScrollTo().performTextInput("Personal mailbox")
            compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
            signInButton("Se connecter avec Forge").performScrollTo().performClick()
            compose.waitUntil(10000) { starts.isNotEmpty() }
            compose.onNodeWithText("Copier le code").assertDoesNotExist()
            capture("plugin-oauth-sign-in")
            compose.waitUntil(20000) {
                compose
                    .onAllNodes(hasText("Enregistrer") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { saved.isNotEmpty() }
            assertEquals(
                "public-client",
                starts.single()["settings"]?.jsonObject?.get("clientId")?.jsonPrimitive?.content,
            )
            assertEquals(
                "synthetic-secret",
                starts.single()["secrets"]?.jsonObject?.get("clientSecret")?.jsonPrimitive?.content,
            )
            assertEquals("oauth-one", saved.single()["credentialFlow"]?.jsonPrimitive?.content)
            assertFalse("token" in saved.single())
        }
    }

    @Test
    fun anAccountSignsInWithADeviceCodeAndSavesTheSignInNotAToken() {
        MockWebServer().use { server ->
            val forms = CopyOnWriteArrayList<JsonObject>()
            val saved = CopyOnWriteArrayList<String>()
            val requests = CopyOnWriteArrayList<String>()
            val started = AtomicInteger()
            server.dispatcher =
                dispatcher(
                    includeGet = true,
                    packages = listOf(onePassword, discord, forge),
                    requests = requests,
                ) { path, body ->
                    val route = path.removePrefix("/api/installations/fixture/api")
                    when {
                        route == "/plugins/forge/form" -> {
                            forms += wireJson.parseToJsonElement(body).jsonObject
                            "{}"
                        }
                        route == "/plugins/forge/device-flow" ->
                            deviceFlow("flow-${started.incrementAndGet()}", "pending")
                        // The first sign-in is cancelled; the second one completes.
                        route == "/plugins/forge/device-flow/flow-1" ->
                            deviceFlow("flow-1", "pending")
                        route == "/plugins/forge/device-flow/flow-2" ->
                            deviceFlow("flow-2", "completed")
                        route == "/plugins/forge/connections" && body.isNotEmpty() -> {
                            saved += body
                            "{}"
                        }
                        else -> null
                    }
                }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, "forge") {} }
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("Ajouter · Account").fetchSemanticsNodes().isNotEmpty()
            }
            // No account yet: the plugin page says what connecting takes and starts the assistant.
            compose.onNodeWithTag("plugin-first-setup").assertExists()
            compose.onNodeWithText("Se connecter").assertExists()
            compose.onNodeWithContentDescription("1. Se connecter avec Forge").assertExists()
            capture("plugin-first-setup")
            compose.onNodeWithText("Commencer la configuration").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Se connecter avec Forge")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Nom").performTextInput("Work")
            // Pasting a token stays possible; without either, an account cannot be saved.
            compose.onNodeWithText("Forge token").assertExists()
            compose.onNodeWithText("Enregistrer").assertIsNotEnabled()

            signInButton("Se connecter avec Forge").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("WXYZ-1234").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Copier le code").assertExists()
            compose.onNodeWithText("Ouvrir Forge").assertExists()
            capture("plugin-device-sign-in")
            compose.onNodeWithText("Annuler la connexion").performScrollTo().performClick()
            compose.waitUntil(10000) {
                requests.any { it.startsWith("DELETE ") && it.endsWith("/device-flow/flow-1") }
            }
            compose.onNodeWithText("WXYZ-1234").assertDoesNotExist()

            signInButton("Se connecter avec Forge").performScrollTo().performClick()
            compose.waitUntil(20000) {
                compose
                    .onAllNodesWithText("Connecté. Enregistrez pour garder ce account.")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            // The package's form handler hears about the new credential through the sign-in.
            compose.waitUntil(10000) {
                forms.any {
                    it["changed"]?.jsonPrimitive?.content == "credential" &&
                        it["credentialFlow"]?.jsonPrimitive?.content == "flow-2"
                }
            }
            assertTrue(forms.none { "token" in it })
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Enregistrer") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { saved.isNotEmpty() }
            val payload = wireJson.parseToJsonElement(saved.single()).jsonObject
            assertEquals("flow-2", payload["credentialFlow"]?.jsonPrimitive?.content)
            assertFalse("token" in payload)
            // The save consumed the sign-in: nothing is cancelled afterwards.
            compose.waitForIdle()
            assertFalse(requests.any { it.startsWith("DELETE ") && it.endsWith("/flow-2") })
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
    fun programReleasesCanBeCheckedInstalledAndRestored() {
        MockWebServer().use { server ->
            val initial = InstalledProgram(release = ProgramRelease(version = "2.39.0"))
            val program =
                AtomicReference(
                    ProgramInstallation(
                        name = "op",
                        installer = "programs/op.luau",
                        installed = initial,
                    )
                )
            val actions = CopyOnWriteArrayList<String>()
            val polls = AtomicInteger()
            server.dispatcher =
                dispatcher(includeGet = true) { path, body ->
                    val base = "/api/installations/fixture/api/plugins/onepassword/programs"
                    when (path) {
                        "/api/installations/fixture/api/plugins/packages/operations/program-job" ->
                            if (polls.incrementAndGet() % 2 == 1) "{\"state\":\"running\"}"
                            else "{\"state\":\"completed\"}"
                        base -> wireJson.encodeToString(ProgramInstallations(listOf(program.get())))
                        "$base/op/check" -> {
                            actions += "check"
                            program.set(
                                program
                                    .get()
                                    .copy(
                                        candidate =
                                            ProgramRelease(
                                                version = "2.41.0",
                                                url = "https://cache.agilebits.com/op.zip",
                                            ),
                                        digest = "reviewed-release",
                                    )
                            )
                            "{\"operation\":\"program-job\"}"
                        }
                        "$base/op/install" -> {
                            actions += "install"
                            assertEquals(
                                "reviewed-release",
                                wireJson
                                    .parseToJsonElement(body)
                                    .jsonObject["digest"]
                                    ?.jsonPrimitive
                                    ?.content,
                            )
                            program.set(
                                program
                                    .get()
                                    .copy(
                                        installed =
                                            InstalledProgram(
                                                release = ProgramRelease(version = "2.41.0")
                                            ),
                                        previous = initial,
                                        candidate = null,
                                        digest = null,
                                    )
                            )
                            "{\"operation\":\"program-job\"}"
                        }
                        "$base/op/rollback" -> {
                            actions += "rollback"
                            program.set(program.get().copy(installed = initial, previous = null))
                            "{\"operation\":\"program-job\"}"
                        }
                        else -> null
                    }
                }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, "onepassword") {} }
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("Vérifier op").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Vérifier op").performScrollTo().performClick()
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("Installer op 2.41.0").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Installer op 2.41.0").performScrollTo().performClick()
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("op · Version 2.41.0").fetchSemanticsNodes().isNotEmpty()
            }
            capture("plugin-program-installed")
            compose.onNodeWithText("Restaurer op 2.39.0").performScrollTo().performClick()
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("op · Version 2.39.0").fetchSemanticsNodes().isNotEmpty()
            }
            assertEquals(listOf("check", "install", "rollback"), actions.toList())
            assertEquals(6, polls.get())
            capture("plugin-program-restored")
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
            compose.onNodeWithText("Gérer sur les fiches agents ›").assertExists()
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
            compose.onNodeWithText("op", useUnmergedTree = true).assertExists()
            capture("plugin-onepassword-agents")
        }
    }

    @Test
    fun serviceAccountsSendTheirTokenOnceAndAreGrantedToTheAgentsChosen() {
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
            compose.onNodeWithText("Continuer").assertIsNotEnabled()
            // What to do on 1Password first, ticked off as the owner goes.
            compose.onNodeWithText("1. Create a service account").assertExists()
            compose.onNodeWithText("Open the guide").assertExists()
            compose.onNodeWithContentDescription("Create a service account : fait").performClick()
            compose
                .onNode(hasSetTextAction() and hasText("Service account token"))
                .performTextInput("ops_android_fixture")
            compose
                .onNode(SemanticsMatcher.keyIsDefined(SemanticsProperties.Password))
                .assertExists()
            capture("plugin-setup-checklist")
            continueSetup()
            // A new account is granted to the agents ticked here; later on, on agent pages.
            compose.onNode(hasText("Helper") and isToggleable()).assertIsEnabled().performClick()
            compose.onNode(hasText("Helper") and isToggleable()).assertIsOn()
            compose.onNodeWithText("Nom").performScrollTo().performTextInput("Staging")
            capture("plugin-instance-editor")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { saved.isNotEmpty() }
            val payload = wireJson.parseToJsonElement(saved.single()).jsonObject
            assertEquals("Staging", payload["name"]?.jsonPrimitive?.content)
            assertEquals(
                buildJsonObject { put("token", "ops_android_fixture") },
                payload["secrets"],
            )
            assertEquals(
                JsonArray(listOf(JsonPrimitive("08c10fb1-ed4e-5f3d-9592-dbfcefd1cdd3"))),
                payload["agentIds"],
            )
            assertEquals(JsonObject(emptyMap()), payload["settings"])
        }
    }

    @Test
    fun anAgentAlreadyUsingAnotherGitHubAccountCannotBeGrantedASecond() {
        // Runs receive the account's git credentials: one account per agent.
        val github =
            forge.copy(
                setup =
                    forge.setup!!.copy(
                        steps = forge.setup.steps + SetupStep("agents", "agents", "Agents")
                    ),
                manifest =
                    requireNotNull(forge.manifest)
                        .copy(
                            agentAccess = PluginAgentAccess(),
                            runCredentials = buildJsonObject { putJsonArray("git") {} },
                        ),
            )
        val work =
            PluginConnection(
                id = "work",
                pluginId = "forge",
                name = "Work",
                agentIds = listOf("08c10fb1-ed4e-5f3d-9592-dbfcefd1cdd3"),
            )
        MockWebServer().use { server ->
            val saved = CopyOnWriteArrayList<JsonObject>()
            server.dispatcher =
                dispatcher(packages = listOf(github), connections = listOf(work)) { path, body ->
                    when {
                        path.endsWith("/plugins/forge/form") -> "{}"
                        path.endsWith("/plugins/forge/connections") -> {
                            saved += wireJson.parseToJsonElement(body).jsonObject
                            "{}"
                        }
                        else -> null
                    }
                }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, "forge") {} }
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("Ajouter · Account").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Ajouter · Account").performClick()
            compose
                .onNode(hasSetTextAction() and hasText("Forge token"))
                .performTextInput("pasted_android_token")
            continueSetup()
            compose.onNodeWithText("Utilise Work").assertExists()
            compose.onNode(hasText("Helper") and isToggleable()).assertIsNotEnabled()
            compose.onNode(hasText("Cairn") and isToggleable()).assertIsEnabled()
            compose.onNodeWithText("Nom").performScrollTo().performTextInput("Personal")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { saved.isNotEmpty() }
            // Skipping the agents step grants nobody.
            assertFalse("agentIds" in saved.single())
            assertEquals("pasted_android_token", saved.single()["token"]?.jsonPrimitive?.content)
        }
    }

    /** The Gmail package's setup, as the installation resolves it, trimmed to three switches. */
    private val gmail =
        PluginEntry(
            id = "gmail",
            builtin = true,
            active = "1.0.0",
            manifest =
                PluginManifest(
                    id = "gmail",
                    name = "Gmail",
                    version = "1.0.0",
                    capabilities = listOf("http"),
                    instances = PluginInstances("Account", "Accounts"),
                    credential =
                        PluginCredential(
                            "Google authorization",
                            "authorization",
                            "Bearer ",
                            oauth =
                                buildJsonObject {
                                    put("clientIdSetting", "clientId")
                                    put("clientSecretSetting", "clientSecret")
                                    putJsonObject("scopeSettings") {
                                        putJsonArray("allowSend") {
                                            add("https://www.googleapis.com/auth/gmail.send")
                                        }
                                        putJsonArray("allowDrafts") {
                                            add("https://www.googleapis.com/auth/gmail.compose")
                                        }
                                        putJsonArray("allowDelete") {
                                            add("https://mail.google.com/")
                                        }
                                    }
                                },
                        ),
                    instanceForm = true,
                    agentAccess = PluginAgentAccess("Off by default."),
                    settings =
                        listOf(
                            PluginSetting("clientId", "Google OAuth client ID", required = true),
                            PluginSetting(
                                "clientSecret",
                                "Google OAuth client secret",
                                type = "secret",
                                required = true,
                            ),
                            PluginSetting("email", "Connected email", hidden = true),
                            PluginSetting(
                                "allowSend",
                                "Send messages",
                                type = "boolean",
                                risk = "medium",
                            ),
                            PluginSetting(
                                "allowDrafts",
                                "Create and edit drafts",
                                type = "boolean",
                                risk = "low",
                            ),
                            PluginSetting(
                                "allowDelete",
                                "Permanently delete mail",
                                type = "boolean",
                                risk = "high",
                            ),
                        ),
                ),
            setup =
                PluginSetup(
                    effort = "app",
                    summary = "Gmail connects through a Google Cloud app you own.",
                    nameFrom = "email",
                    steps =
                        listOf(
                            SetupStep(
                                "app",
                                title = "Google Cloud app",
                                settings = listOf("clientId", "clientSecret"),
                                shared = true,
                                checklist =
                                    listOf(
                                        SetupTask(
                                            "Enable the Gmail API",
                                            url =
                                                "https://console.cloud.google.com/apis/library/gmail.googleapis.com",
                                            link = "Open API Library",
                                        ),
                                        SetupTask(
                                            "Create an OAuth client",
                                            "Add this address.",
                                            copy = "callbackUrl",
                                        ),
                                    ),
                                warning = "A Testing app expires after seven days.",
                            ),
                            SetupStep(
                                "permissions",
                                title = "Permissions",
                                settings = listOf("allowSend", "allowDrafts", "allowDelete"),
                                presets =
                                    listOf(
                                        SetupPreset("read", "Read only", risk = "low"),
                                        SetupPreset(
                                            "send",
                                            "Read, draft and send",
                                            risk = "medium",
                                            values =
                                                mapOf("allowDrafts" to true, "allowSend" to true),
                                        ),
                                    ),
                            ),
                            SetupStep("connect", "credential", "Sign in with Google"),
                            SetupStep("agents", "agents", "Agents"),
                        ),
                ),
        )

    @Test
    fun aMailboxReusesTheGoogleCloudAppAndIsNamedAfterItsSignIn() {
        val clientId = "481203947512-k2d9qv0bm3h1.apps.googleusercontent.com"
        val alice =
            PluginConnection(
                id = "alice",
                pluginId = "gmail",
                name = "alice@example.test",
                settings =
                    buildJsonObject {
                        put("clientId", clientId)
                        put("email", "alice@example.test")
                    },
                secretKeys = listOf("clientSecret"),
            )
        MockWebServer().use { server ->
            val forms = CopyOnWriteArrayList<JsonObject>()
            val starts = CopyOnWriteArrayList<JsonObject>()
            val saved = CopyOnWriteArrayList<JsonObject>()
            val flow =
                PluginDeviceFlow(
                    "oauth-g",
                    "gmail",
                    "pending",
                    "",
                    "https://accounts.google.com/o",
                    expiresAt = 1,
                )
            server.dispatcher =
                dispatcher(
                    includeGet = true,
                    packages = listOf(gmail),
                    connections = listOf(alice),
                ) { path, body ->
                    when (path.removePrefix("/api/installations/fixture/api")) {
                        "/plugins/gmail/form" -> {
                            val request = wireJson.parseToJsonElement(body).jsonObject
                            forms += request
                            // The package verifies the mailbox once signed in.
                            if ("credentialFlow" in request)
                                buildJsonObject {
                                    putJsonObject("values") { put("email", "bob@example.test") }
                                }
                                    .toString()
                            else "{}"
                        }
                        "/plugins/gmail/oauth" ->
                            if (body.isEmpty())
                                buildJsonObject {
                                    put(
                                        "callbackUrl",
                                        "https://cairn.example/plugins/oauth/callback",
                                    )
                                }
                                    .toString()
                            else {
                                starts += wireJson.parseToJsonElement(body).jsonObject
                                wireJson.encodeToString(flow)
                            }
                        "/plugins/gmail/oauth/oauth-g" ->
                            wireJson.encodeToString(flow.copy(state = "completed"))
                        "/plugins/gmail/connections" -> {
                            saved += wireJson.parseToJsonElement(body).jsonObject
                            "{}"
                        }
                        else -> null
                    }
                }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, "gmail") {} }
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("Ajouter · Account").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Ajouter · Account").performClick()
            // The app saved with Alice's mailbox is offered first; its secret stays on the server.
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Réutiliser la configuration de alice@example.test")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText(shortValue(clientId, 14)).assertExists()
            compose.onNodeWithText("1. Enable the Gmail API").assertDoesNotExist()
            capture("plugin-gmail-reuse")
            // Another app: its checklist, the callback URL to copy and Google's warning.
            compose.onNodeWithText("En configurer une nouvelle").performClick()
            compose.onNodeWithText("1. Enable the Gmail API").assertExists()
            compose.onNodeWithText("Open API Library").assertExists()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Copier l’adresse de retour")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("A Testing app expires after seven days.").assertExists()
            compose.onNodeWithText("Continuer").assertIsNotEnabled()
            capture("plugin-gmail-app")
            compose
                .onNodeWithText("Réutiliser la configuration de alice@example.test")
                .performScrollTo()
                .performClick()
            continueSetup()
            // Permissions as presets; the switches stay at hand.
            compose.onNode(hasText("Read only") and isSelected()).assertExists()
            // Alice's card behind the assistant lists the same settings: look at switches only.
            compose.onNode(hasText("Send messages") and isToggleable()).assertDoesNotExist()
            compose.onNodeWithText("Read, draft and send").performClick()
            compose.onNode(hasText("Read, draft and send") and isSelected()).assertExists()
            compose.onNodeWithText("Affiner").performClick()
            compose.onNode(hasText("Send messages") and isToggleable()).assertIsOn()
            compose.onNode(hasText("Permanently delete mail") and isToggleable()).assertIsOff()
            compose.onNodeWithText("Risque élevé").assertExists()
            capture("plugin-gmail-permissions")
            continueSetup()
            compose.onNodeWithText("Enregistrer").assertDoesNotExist()
            signInButton("Sign in with Google").performScrollTo().performClick()
            compose.waitUntil(20000) {
                compose.onAllNodesWithText("Connecté.").fetchSemanticsNodes().isNotEmpty()
            }
            // Other permissions after signing in need a new sign-in, which the installation
            // would require: even choosing the same ones again.
            compose.onNodeWithText("Retour").performClick()
            compose.onNodeWithText("Read only").performClick()
            compose.onNodeWithText("Read, draft and send").performClick()
            continueSetup()
            compose.onNodeWithText("Connecté.").assertDoesNotExist()
            compose.onNodeWithText("Continuer").assertIsNotEnabled()
            signInButton("Sign in with Google").performScrollTo().performClick()
            compose.waitUntil(20000) {
                compose.onAllNodesWithText("Connecté.").fetchSemanticsNodes().isNotEmpty()
            }
            continueSetup()
            compose.onNode(hasText("Helper") and isToggleable()).performClick()
            // Named after the mailbox the package verified.
            compose.onNode(hasSetTextAction() and hasText("bob@example.test")).assertExists()
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { saved.isNotEmpty() }
            val payload = saved.single()
            assertEquals("bob@example.test", payload["name"]?.jsonPrimitive?.content)
            assertEquals("alice", payload["sharedFrom"]?.jsonPrimitive?.content)
            assertEquals("oauth-g", payload["credentialFlow"]?.jsonPrimitive?.content)
            assertEquals(
                JsonArray(listOf(JsonPrimitive("08c10fb1-ed4e-5f3d-9592-dbfcefd1cdd3"))),
                payload["agentIds"],
            )
            assertEquals(
                buildJsonObject {
                    put("clientId", clientId)
                    put("email", "bob@example.test")
                    put("allowSend", true)
                    put("allowDrafts", true)
                    put("allowDelete", false)
                },
                payload["settings"],
            )
            assertEquals(JsonObject(emptyMap()), payload["secrets"])
            assertEquals(2, starts.size)
            assertTrue(starts.all { it["sharedFrom"]?.jsonPrimitive?.content == "alice" })
            // The package hears about the reused app too, never its secret.
            val signedIn = forms.filter { "credentialFlow" in it }
            assertTrue(forms.toString(), signedIn.isNotEmpty())
            assertTrue(signedIn.all { it["sharedFrom"]?.jsonPrimitive?.content == "alice" })
            assertTrue(forms.none { it["secrets"]?.jsonObject?.isNotEmpty() == true })
        }
    }

    /** Waits for the package's answer, then goes to the next step. */
    private fun continueSetup() {
        compose.waitUntil(10000) {
            compose
                .onAllNodes(hasText("Continuer") and isEnabled())
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
        compose.onNodeWithText("Continuer").performClick()
    }

    /** The sign-in button, not the step's title above it. */
    private fun signInButton(label: String) = compose.onNode(hasText(label) and hasClickAction())

    @Test fun editingAnInstanceLeavesItsGrantsToTheAgentPages() = editExistingAccount()

    @Test
    fun aGrantRevokedWhileEditingIsNotRestored() = editExistingAccount(revokedMeanwhile = true)

    private fun editExistingAccount(revokedMeanwhile: Boolean = false) {
        MockWebServer().use { server ->
            val saved = CopyOnWriteArrayList<String>()
            val editing = java.util.concurrent.atomic.AtomicBoolean(false)
            server.dispatcher =
                dispatcher(includeGet = true) { path, body ->
                    when {
                        path.endsWith("/plugins/onepassword/connections/op1") -> {
                            saved += body
                            "{}"
                        }
                        // Revoked from an agent page while the editor is open.
                        path == "/api/installations/fixture/api/plugins" &&
                            revokedMeanwhile &&
                            editing.get() ->
                            wireJson.encodeToString(
                                PluginCatalog(
                                    packages = listOf(onePassword, discord, notes),
                                    connections = listOf(bot, account.copy(agentIds = emptyList())),
                                )
                            )
                        else -> null
                    }
                }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, "onepassword") {} }
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("Modifier").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Modifier").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Enregistrer").fetchSemanticsNodes().isNotEmpty()
            }
            editing.set(true)
            compose.onNodeWithText("Enregistrer").performClick()
            // Keep the clock moving while the save completes.
            repeat(100) {
                if (saved.isEmpty()) {
                    compose.mainClock.advanceTimeBy(100)
                    androidx.test.platform.app.InstrumentationRegistry.getInstrumentation()
                        .waitForIdleSync()
                    Thread.sleep(50)
                }
            }
            compose.waitUntil(10000) { saved.isNotEmpty() }
            val payload = wireJson.parseToJsonElement(saved.single()).jsonObject
            assertFalse("agentIds" in payload)
        }
    }

    /** Answers the workspace fixture; custom handlers can opt into GETs for polling. */
    private fun dispatcher(
        includeGet: Boolean = false,
        packages: List<PluginEntry> = listOf(onePassword, discord, notes),
        connections: List<PluginConnection> = listOf(bot, account),
        /** Every request as "METHOD path", when a journey checks methods. */
        requests: MutableList<String>? = null,
        handle: (String, String) -> String?,
    ) =
        object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse {
                val path = request.path.orEmpty()
                val body = request.body.readUtf8()
                requests?.add("${request.method} $path")
                if (request.method != "GET") {
                    if (request.getHeader("X-CSRF-Token") != "fixture-csrf")
                        return MockResponse().setResponseCode(403)
                }
                if (request.method != "GET" || includeGet) {
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
                                build.cairn.contracts.ListAgentsOutput(
                                    listOf(
                                        Agent(
                                            exclusiveNodes = emptyList(),
                                            plugins = emptyList(),
                                            createdAt = 0,
                                            access =
                                                AccessPolicy(
                                                    mcpTools =
                                                        build.cairn.contracts.McpTools(emptyMap()),
                                                    projects = null,
                                                    skills = null,
                                                    mcps = null,
                                                ),
                                            id = MAIN_AGENT_ID,
                                            name = "Cairn",
                                            revision = "r1",
                                        ),
                                        Agent(
                                            exclusiveNodes = emptyList(),
                                            plugins = emptyList(),
                                            createdAt = 0,
                                            id = "08c10fb1-ed4e-5f3d-9592-dbfcefd1cdd3",
                                            name = "Helper",
                                            access =
                                                AccessPolicy(
                                                    mcpTools =
                                                        build.cairn.contracts.McpTools(emptyMap()),
                                                    skills = null,
                                                    mcps = null,
                                                    projects = listOf("project"),
                                                ),
                                            revision = "r1",
                                        ),
                                    )
                                )
                            )
                        "/api/installations/fixture/api/plugins" ->
                            wireJson.encodeToString(
                                PluginCatalog(packages = packages, connections = connections)
                            )
                        "/api/installations/fixture/api/plugins/onepassword/programs" ->
                            wireJson.encodeToString(
                                ProgramInstallations(
                                    listOf(
                                        ProgramInstallation(
                                            name = "op",
                                            installer = "programs/op.luau",
                                            installed =
                                                InstalledProgram(
                                                    release = ProgramRelease(version = "2.39.0")
                                                ),
                                        )
                                    )
                                )
                            )
                        "/api/installations/fixture/api/plugins/discord/connections/bot/view" ->
                            """{"view":[{"type":"facts","items":[{"label":"Channel","value":"#support"},{"label":"Server","value":"Acme"}]}]}"""
                        "/api/installations/fixture/api/plugins/onepassword/connections/op1/view" ->
                            """{"view":[{"type":"section","title":"Vaults","children":[{"type":"actions","children":[{"type":"button","label":"Show vaults","action":"vaults"}]}]}]}"""
                        "/api/installations/fixture/api/overview",
                        "/api/installations/fixture/api/codex/models" -> "{}"
                        else ->
                            if (request.requestUrl?.encodedPath?.endsWith("/api/projects") == true)
                                """{"projects":[]}"""
                            else if (
                                request.requestUrl?.encodedPath?.endsWith("/api/agents") == true
                            )
                                """{"agents":[]}"""
                            else if (request.requestUrl?.encodedPath?.endsWith("/api/tasks") == true) """{"tasks":[]}""" else "[]"
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
