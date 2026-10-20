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
    fun requiredWhitespaceChoiceCanBeSavedWithNonblankUnicodeText() {
        val entry =
            notes.copy(
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
                    )
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
    fun serviceAccountsSendTheirTokenOnceAndLeaveGrantsToAgentPages() {
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
            // Grants are read-only here: they are set on each agent's page.
            compose.onNodeWithText("Aucun agent ne peut l’utiliser pour l’instant.").assertExists()
            assertTrue(
                compose
                    .onAllNodesWithText("Gérer sur les fiches agents ›")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            )
            compose.onNode(hasText("Helper") and isToggleable()).assertDoesNotExist()
            capture("plugin-instance-editor")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { saved.isNotEmpty() }
            val payload = wireJson.parseToJsonElement(saved.single()).jsonObject
            assertEquals("Staging", payload["name"]?.jsonPrimitive?.content)
            assertEquals(
                buildJsonObject { put("token", "ops_android_fixture") },
                payload["secrets"],
            )
            assertFalse("agentIds" in payload)
            assertEquals(JsonObject(emptyMap()), payload["settings"])
        }
    }

    @Test fun editingAnInstanceLeavesItsGrantsToTheAgentPages() = editExistingAccount(older = false)

    @Test
    fun anOlderInstallationKeepsTheGrantsOfAnEditedInstance() = editExistingAccount(older = true)

    @Test
    fun aGrantRevokedWhileEditingIsNotRestored() =
        editExistingAccount(older = true, revokedMeanwhile = true)

    /**
     * An older installation (agents without `grantsOnAgent`) reads a missing `agentIds` as "no
     * agents": the editor then sends the instance's current grants unchanged. A current one keeps
     * them.
     */
    private fun editExistingAccount(older: Boolean, revokedMeanwhile: Boolean = false) {
        MockWebServer().use { server ->
            val saved = CopyOnWriteArrayList<String>()
            val editing = java.util.concurrent.atomic.AtomicBoolean(false)
            server.dispatcher =
                dispatcher(includeGet = true, current = !older) { path, body ->
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
            // The save reads the instance again first; keep the clock moving for its reply.
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
            if (revokedMeanwhile) assertFalse("agentIds" in payload)
            else if (older)
                assertEquals(JsonArray(listOf(JsonPrimitive(MAIN_AGENT_ID))), payload["agentIds"])
            else assertFalse("agentIds" in payload)
        }
    }

    /** Answers the workspace fixture; custom handlers can opt into GETs for polling. */
    private fun dispatcher(
        includeGet: Boolean = false,
        /** False for an installation that does not mark agents with `grantsOnAgent`. */
        current: Boolean = true,
        handle: (String, String) -> String?,
    ) =
        object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse {
                val path = request.path.orEmpty()
                val body = request.body.readUtf8()
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
                                listOf(
                                    Agent(
                                        id = MAIN_AGENT_ID,
                                        name = "Cairn",
                                        revision = "r1",
                                        grantsOnAgent = current,
                                    ),
                                    Agent(
                                        id = "helper",
                                        name = "Helper",
                                        access = AccessPolicy(projects = listOf("project")),
                                        revision = "r1",
                                        grantsOnAgent = current,
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
