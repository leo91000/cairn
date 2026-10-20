package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.*
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
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

    @Test
    fun pluginCatalogueResumesImmediatelyWhenWorkspaceBecomesIdle() {
        val blocked = mutableStateOf(true)
        val composed = CountDownLatch(1)
        val firstRead = CountDownLatch(1)
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        if (path.endsWith("/plugins")) firstRead.countDown()

                        null
                    }
                ) { _, _ ->
                    null
                }
            server.start()
            val vm =
                show(server) { model, state ->
                    PluginScreen(model, state.copy(busy = blocked.value), "onepassword") {}
                    SideEffect { composed.countDown() }
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                compose.waitUntil(10000) { composed.count == 0L }
                compose.waitForIdle()
                assertEquals(1L, firstRead.count)
                compose.runOnIdle { blocked.value = false }
                compose.waitUntil(2500) {
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    firstRead.count == 0L
                }
                compose.waitUntil(10000) {
                    compose.onAllNodesWithText("1Password").fetchSemanticsNodes().isNotEmpty()
                }
            } finally {
                models.clear()
            }
        }
    }

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
                            path.endsWith("$root/setup1") -> draft.get()?.toString()
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
    fun setupDraftsLoadEveryPageBeforeResumingAndDiscardingTheLastDraft() {
        val entry =
            PluginEntry(
                id = "draft-pages",
                active = "1",
                manifest =
                    PluginManifest(
                        id = "draft-pages",
                        name = "Draft pages",
                        version = "1",
                        settings =
                            listOf(PluginSetting(key = "label", title = "Label", type = "string")),
                    ),
            )
        val drafts = CopyOnWriteArrayList<PluginSetupDraft>()
        for ((index, name) in listOf("First", "Last").withIndex()) {
            drafts +=
                PluginSetupDraft(
                    id = "00000000-0000-4000-8000-00000000000${index + 1}",
                    pluginId = "draft-pages",
                    name = "$name setup",
                    revision = "revision-$index",
                    settings = buildJsonObject { put("label", "$name saved value") },
                    expiresAt = System.currentTimeMillis() + 86400000,
                )
        }
        val last = drafts.last()
        val started = CountDownLatch(1)
        val release = CountDownLatch(1)
        val deleted = CopyOnWriteArrayList<String>()
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("/plugins") ->
                                wireJson.encodeToString(PluginCatalog(packages = listOf(entry)))
                            path.endsWith("/setup-drafts/${last.id}") ->
                                wireJson.encodeToString(last)
                            path.substringBefore('?').endsWith("/setup-drafts") -> {
                                val cursor = android.net.Uri.parse(path).getQueryParameter("cursor")
                                if (cursor != null) {
                                    started.countDown()
                                    check(release.await(20, TimeUnit.SECONDS))
                                }
                                val remaining = drafts.filter { cursor == null || it.id > cursor }
                                wireJson.encodeToString(
                                    PluginPage(
                                        remaining.take(1),
                                        remaining.firstOrNull()?.id?.takeIf { remaining.size > 1 },
                                    )
                                )
                            }
                            else -> null
                        }
                    }
                ) { path, body ->
                    if (path.endsWith("/setup-drafts/${last.id}")) {
                        assertEquals(
                            last.revision,
                            wireJson
                                .parseToJsonElement(body)
                                .jsonObject["revision"]!!
                                .jsonPrimitive
                                .content,
                        )
                        deleted += last.id
                        drafts.remove(last)
                        "{}"
                    } else null
                }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, "draft-pages") {} }
            try {
                compose.waitUntil(10000) { started.count == 0L }
                compose.onNodeWithText("Continuer Last setup").assertDoesNotExist()
            } finally {
                release.countDown()
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Continuer Last setup")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Continuer Last setup").performScrollTo().performClick()
            compose.onNodeWithText("Last saved value").assertExists()
            compose.onNodeWithContentDescription("Fermer").performClick()
            compose.onNodeWithText("Abandonner Last setup").performScrollTo().performClick()
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(10000) {
                deleted.size == 1 &&
                    compose
                        .onAllNodesWithText("Abandonner Last setup")
                        .fetchSemanticsNodes()
                        .isEmpty()
            }
            assertEquals(listOf(last.id), deleted.toList())
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
                            path.endsWith("/setup-drafts/setup-feedback1") ->
                                wireJson.encodeToString(draft.get())
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

    @Test
    fun membersCanOpenTheirOwnPluginAccountsWithoutOwnerManagement() {
        MockWebServer().use { server ->
            server.dispatcher = dispatcher { _, _ -> null }
            server.start()
            var destination = ""
            show(server) { vm, state ->
                AtelierScreen(
                    vm,
                    state.copy(
                        installation = state.installation?.copy(role = InstallationRole.Member)
                    ),
                ) {
                    destination = it
                }
            }
            compose.waitUntil(30000) {
                compose
                    .onAllNodesWithText("Atelier", substring = false)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Vos comptes de plugins").performScrollTo().performClick()
            compose.runOnIdle { assertEquals("personal-connections", destination) }
            compose.onNodeWithText("Connexions").assertDoesNotExist()
            compose.onNodeWithText("Plugins", substring = false).assertDoesNotExist()
        }
    }

    @Test
    fun personalAccountRefreshKeepsLaterPageConsentUntilItFinishes() {
        val connected = java.util.concurrent.ConcurrentHashMap.newKeySet<String>()
        val shifted = java.util.concurrent.atomic.AtomicBoolean(false)
        val starts = CopyOnWriteArrayList<String>()
        val finishes = CopyOnWriteArrayList<String>()
        fun item(id: String) =
            PluginPersonalAuthorization(
                id = id,
                pluginId = "personal-pages",
                pluginName = "Personal service",
                instanceId = "app",
                instanceName = "Company service",
                collection = "workspace",
                resourceId = id,
                resourceName = id,
                providerName = "personal",
                title = "Personal access",
                scopes = listOf("messages:read"),
                enabled = true,
                authorization =
                    if (id in connected)
                        PluginOAuthStatus(
                            status = "authorized",
                            scopes = listOf("messages:read"),
                            subject = "U-$id",
                        )
                    else PluginOAuthStatus(),
            )
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        if (
                            path.substringBefore('?').endsWith("/plugins/personal-authorizations")
                        ) {
                            val ids =
                                if (shifted.get()) listOf("Intro", "Engineering", "Sales")
                                else listOf("Engineering", "Sales")
                            val cursor = path.substringAfter("?cursor=", "")
                            val index = if (cursor.isEmpty()) 0 else ids.indexOf(cursor) + 1
                            wireJson.encodeToString(
                                PluginPage(
                                    items =
                                        ids.getOrNull(index)?.let { listOf(item(it)) }.orEmpty(),
                                    nextCursor =
                                        ids.getOrNull(index).takeIf { index + 1 < ids.size },
                                )
                            )
                        } else null
                    }
                ) { path, body ->
                    if ("/personal-pages/" !in path) null
                    else {
                        val id = path.substringAfter("/workspace/").substringBefore('/')
                        val value = wireJson.parseToJsonElement(body).jsonObject
                        if (path.endsWith("/finish")) {
                            assertEquals("consent-$id", value["state"]?.jsonPrimitive?.content)
                            finishes += id
                            connected += id
                            wireJson.encodeToString(item(id).authorization)
                        } else {
                            assertEquals(
                                listOf("messages:read"),
                                value["scopes"]?.jsonArray?.map { it.jsonPrimitive.content },
                            )
                            starts += id
                            wireJson.encodeToString(
                                PluginOAuthConsent(
                                    state = "consent-$id",
                                    url = "https://provider.example.test/authorize?resource=$id",
                                    expiresAt = System.currentTimeMillis() + 600000,
                                )
                            )
                        }
                    }
                }
            server.start()
            show(server) { vm, state -> PersonalPluginAccountsScreen(vm, state) }
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("messages:read").fetchSemanticsNodes().size == 1
            }
            compose.onNodeWithText("messages:read").performScrollTo().performClick()
            compose.onNodeWithText("Connecter", substring = false).performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Vérifier l’autorisation").fetchSemanticsNodes().size ==
                    1
            }
            compose
                .onNodeWithText("Plus de connexions personnelles")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("messages:read").fetchSemanticsNodes().size == 2
            }
            compose.onAllNodesWithText("messages:read")[1].performScrollTo().performClick()
            compose.onNodeWithText("Connecter", substring = false).performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Vérifier l’autorisation").fetchSemanticsNodes().size ==
                    2
            }
            compose
                .onAllNodesWithText("Vérifier l’autorisation")[0]
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Connecté en tant que U-Engineering")
                    .fetchSemanticsNodes()
                    .size == 1 &&
                    compose
                        .onAllNodes(hasText("Actualiser") and isEnabled())
                        .fetchSemanticsNodes()
                        .size == 1
            }
            compose.onNodeWithText("Sales", substring = false).assertExists()
            compose.onNodeWithText("Vérifier l’autorisation", substring = false).assertExists()
            shifted.set(true)
            compose.onNodeWithText("Actualiser", substring = false).performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Intro", substring = false).fetchSemanticsNodes().size ==
                    1
            }
            compose
                .onNodeWithText("Vérifier l’autorisation", substring = false)
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Connecté en tant que U-Sales")
                    .fetchSemanticsNodes()
                    .size == 1
            }
            assertEquals(listOf("Engineering", "Sales"), starts.toList())
            assertEquals(listOf("Engineering", "Sales"), finishes.toList())
            capture("personal-later-page-consent")
        }
    }

    @Test
    fun personalPluginAuthorizationSelectsScopesChecksConsentAndDisconnects() {
        val oauth =
            "/api/installations/fixture/api/plugins/personal-ui/connections/app/resources/workspace/team/oauth/personal"
        val starts = CopyOnWriteArrayList<JsonObject>()
        val finishes = java.util.concurrent.atomic.AtomicInteger(0)
        val connected = java.util.concurrent.atomic.AtomicBoolean(false)
        val disconnected = java.util.concurrent.atomic.AtomicBoolean(false)
        fun status() =
            if (connected.get())
                """{"status":"authorized","scopes":["messages:read"],"requestedScopes":["messages:read"],"subject":"U111"}"""
            else """{"status":"disconnected"}"""
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        if (path.endsWith("/plugins/personal-authorizations"))
                            """{"items":[{"id":"own-authorization","pluginId":"personal-ui","pluginName":"Personal service","instanceId":"app","instanceName":"Company service","collection":"workspace","resourceId":"team","resourceName":"Engineering","providerName":"personal","title":"Personal workspace access","enabled":true,"scopes":["messages:read","messages:write"],"authorization":${status()}}]}"""
                        else null
                    }
                ) { path, body ->
                    when (path) {
                        oauth -> {
                            val parsed = wireJson.parseToJsonElement(body).jsonObject
                            if ("scopes" in parsed) {
                                starts += parsed
                                """{"state":"fixture-state","url":"https://provider.example.test/authorize","expiresAt":${System.currentTimeMillis() + 600000}}"""
                            } else {
                                connected.set(false)
                                disconnected.set(true)
                                status()
                            }
                        }
                        "$oauth/finish" -> {
                            assertEquals(
                                "fixture-state",
                                wireJson
                                    .parseToJsonElement(body)
                                    .jsonObject["state"]
                                    ?.jsonPrimitive
                                    ?.content,
                            )
                            if (finishes.incrementAndGet() == 1) """{"status":"pending"}"""
                            else {
                                connected.set(true)
                                status()
                            }
                        }
                        else -> null
                    }
                }
            server.start()
            show(server) { vm, state -> PersonalPluginAccountsScreen(vm, state) }
            compose.waitUntil(30000) {
                compose
                    .onAllNodesWithText("Connecter", substring = false)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Vos comptes de plugins").assertExists()
            compose.onNodeWithText("Connecter", substring = false).assertIsNotEnabled()
            compose.onNodeWithText("messages:read").performScrollTo().performClick()
            compose.onNodeWithText("Connecter", substring = false).performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Ouvrir la page d’autorisation")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Ouvrir la page d’autorisation").assertExists()
            assertEquals(
                listOf("messages:read"),
                starts.single()["scopes"]?.jsonArray?.map { it.jsonPrimitive.content },
            )
            compose.onNodeWithText("Vérifier l’autorisation").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Terminez l’autorisation dans votre navigateur.")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Terminez l’autorisation dans votre navigateur.").assertExists()
            compose.onNodeWithText("Vérifier l’autorisation").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Connecté en tant que U111")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Connecté en tant que U111").performScrollTo().assertExists()
            capture("personal-plugin-account")
            compose
                .onNodeWithText("Déconnecter", substring = false)
                .performScrollTo()
                .performClick()
            compose.onNodeWithText("Déconnecter Personal workspace access ?").assertExists()
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(10000) { disconnected.get() }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Non connecté", substring = false)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Non connecté", substring = false).assertExists()
            compose.onNodeWithText("Ouvrir la page d’autorisation").assertDoesNotExist()
        }
    }

    /** Answers the workspace fixture; custom reads and writes remain HTTP boundaries. */
    @Test
    fun ownerResourceAuthorizationsGateSetupIndependentlyAndWorkAfterActivation() {
        val manifest =
            PluginManifest(
                id = "owner-ui",
                name = "Owner service",
                version = "1",
                settings =
                    listOf(
                        PluginSetting(
                            key = "callbackOrigin",
                            title = "Callback origin",
                            type = "url",
                        )
                    ),
                resources =
                    listOf(
                        PluginResourceCollection(
                            name = "workspace",
                            title = "Workspaces",
                            singular = "Workspace",
                        )
                    ),
                oauth =
                    listOf(
                        PluginOAuthProvider(
                            name = "owner",
                            title = "Workspace authorization",
                            collection = "workspace",
                            scopes = listOf("messages:read", "messages:write"),
                        ),
                        PluginOAuthProvider(
                            name = "personal",
                            title = "Personal access",
                            collection = "workspace",
                            personal = PluginOAuthPersonal("/person"),
                        ),
                    ),
            )
        val entry = PluginEntry(id = "owner-ui", active = "1", manifest = manifest)
        val draft =
            PluginSetupDraft(
                id = "setup-owner",
                pluginId = "owner-ui",
                name = "Shared service",
                revision = "root1",
                settings = buildJsonObject { put("callbackOrigin", "https://saved.example.test") },
                expiresAt = System.currentTimeMillis() + 600000,
            )
        val active = java.util.concurrent.atomic.AtomicBoolean(false)
        val connected = java.util.concurrent.ConcurrentHashMap.newKeySet<String>()
        val revisions = java.util.concurrent.ConcurrentHashMap<String, Int>()
        val finishes = java.util.concurrent.atomic.AtomicInteger(0)
        val shifted = java.util.concurrent.atomic.AtomicBoolean(false)
        val writes = CopyOnWriteArrayList<Pair<String, JsonObject>>()
        fun resource(id: String) =
            PluginResource(
                id = id,
                connectionId = draft.id,
                collection = "workspace",
                name =
                    when (id) {
                        "intro" -> "Intro"
                        "engineering" -> "Engineering"
                        else -> "Sales"
                    },
                revision = "resource${revisions[id] ?: 0}",
            )
        fun status(id: String) =
            if (id in connected)
                """{"status":"authorized","scopes":["messages:read"],"requestedScopes":["messages:read"]}"""
            else """{"status":"disconnected"}"""
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("/plugins") ->
                                wireJson.encodeToString(
                                    PluginCatalog(
                                        packages = listOf(entry),
                                        connections =
                                            if (active.get())
                                                listOf(
                                                    PluginConnection(
                                                        id = draft.id,
                                                        pluginId = manifest.id,
                                                        name = draft.name,
                                                        status = PluginStatus("ready"),
                                                    )
                                                )
                                            else emptyList(),
                                    )
                                )
                            path.endsWith("/setup-drafts/${draft.id}") ->
                                wireJson.encodeToString(draft)
                            path.endsWith("/setup-drafts") ->
                                wireJson.encodeToString(
                                    PluginPage(if (active.get()) emptyList() else listOf(draft))
                                )
                            path.substringBefore('?').endsWith("/resources/workspace") ->
                                wireJson.encodeToString(
                                    if ("?cursor=" in path) PluginPage(listOf(resource("sales")))
                                    else
                                        PluginPage(
                                            if (shifted.get())
                                                listOf(resource("intro"), resource("engineering"))
                                            else listOf(resource("engineering")),
                                            "engineering",
                                        )
                                )
                            path.endsWith("/oauth/owner") ->
                                status(
                                    path
                                        .substringAfter("/resources/workspace/")
                                        .substringBefore('/')
                                )
                            else -> null
                        }
                    }
                ) { path, body ->
                    val input = wireJson.parseToJsonElement(body).jsonObject
                    writes += path to input
                    when {
                        path.endsWith("/activate") -> {
                            active.set(true)
                            wireJson.encodeToString(
                                PluginConnection(
                                    id = draft.id,
                                    pluginId = manifest.id,
                                    name = draft.name,
                                    status = PluginStatus("ready"),
                                )
                            )
                        }
                        path.endsWith("/oauth/owner/finish") -> {
                            if (finishes.incrementAndGet() == 1) """{"status":"pending"}"""
                            else {
                                connected.add("engineering")
                                revisions.merge("engineering", 1, Int::plus)
                                status("engineering")
                            }
                        }
                        path.endsWith("/oauth/owner") -> {
                            val id =
                                path.substringAfter("/resources/workspace/").substringBefore('/')
                            if ("scopes" in input)
                                """{"state":"fixture-state-$id","url":"https://provider.example.test/authorize","redirectUri":"${input["directOrigin"]?.jsonPrimitive?.content ?: draft.settings["callbackOrigin"]!!.jsonPrimitive.content}/callback","expiresAt":${System.currentTimeMillis() + 600000}}"""
                            else {
                                connected.remove(id)
                                revisions.merge(id, 1, Int::plus)
                                status(id)
                            }
                        }
                        else -> null
                    }
                }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, manifest.id) {} }
            compose.waitUntil(30000) {
                compose
                    .onAllNodesWithText("Continuer Shared service")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Continuer Shared service").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Connecter", substring = false)
                    .fetchSemanticsNodes()
                    .size == 1
            }
            compose.onNodeWithText("Personal access").assertDoesNotExist()
            compose.onAllNodesWithText("Connecter", substring = false)[0].assertIsNotEnabled()
            compose.onAllNodesWithText("messages:read")[0].performScrollTo().performClick()
            compose
                .onAllNodesWithText("Origine HTTPS directe (facultative)")[0]
                .performScrollTo()
                .performTextInput("https://cairn.example.test")
            compose
                .onAllNodesWithText("Connecter", substring = false)[0]
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Vérifier l’autorisation").fetchSemanticsNodes().size ==
                    1
            }
            compose.onNodeWithText("Voir plus · Workspaces").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("messages:read").fetchSemanticsNodes().size == 2
            }
            compose.onAllNodesWithText("messages:read")[1].performScrollTo().performClick()
            compose.onNodeWithText("Connecter", substring = false).performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Vérifier l’autorisation").fetchSemanticsNodes().size ==
                    2
            }
            compose.onNodeWithText("Actualiser les ressources").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Vérifier l’autorisation") and isEnabled())
                    .fetchSemanticsNodes()
                    .size == 2
            }
            val starts = writes.filter { it.second.containsKey("scopes") }
            assertEquals(2, starts.size)
            assertEquals("root1", starts[0].second["setupRevision"]?.jsonPrimitive?.content)
            assertEquals(
                "https://cairn.example.test",
                starts[0].second["directOrigin"]?.jsonPrimitive?.content,
            )
            assertFalse(starts[1].second.containsKey("directOrigin"))
            compose
                .onNodeWithText("URL de retour : https://saved.example.test/callback")
                .assertExists()
            compose
                .onNodeWithText("Enregistrer la configuration")
                .performScrollTo()
                .assertIsNotEnabled()
            compose.onNodeWithText("Activer l’instance").performScrollTo().assertIsNotEnabled()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Vérifier l’autorisation") and isEnabled())
                    .fetchSemanticsNodes()
                    .size == 2
            }
            compose
                .onAllNodes(hasText("Vérifier l’autorisation") and isEnabled())[0]
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) { finishes.get() == 1 }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Terminez l’autorisation dans votre navigateur.")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose
                .onAllNodesWithText("Vérifier l’autorisation")[0]
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Connecté", substring = false)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Actualiser les ressources") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Sales", substring = false).assertExists()
            compose.onNodeWithText("Vérifier l’autorisation", substring = false).assertExists()
            // The changed resource also reloads its status after its revision changes.
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Déconnecter", substring = false)
                    .fetchSemanticsNodes()
                    .size == 2
            }
            // Another owner inserts a resource before the open, unchanged consent.
            shifted.set(true)
            compose.onNodeWithText("Actualiser les ressources").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Intro", substring = false)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Vérifier l’autorisation", substring = false)
                    .fetchSemanticsNodes()
                    .size == 1
            }
            compose.onNodeWithText("Activer l’instance").performScrollTo().assertIsNotEnabled()
            compose
                .onAllNodesWithText("Déconnecter", substring = false)[1]
                .performScrollTo()
                .performClick()
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Activer l’instance") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty() &&
                    compose
                        .onAllNodesWithText("Vérifier l’autorisation")
                        .fetchSemanticsNodes()
                        .isEmpty()
            }
            compose
                .onNodeWithText("Activer l’instance")
                .performScrollTo()
                .assertIsEnabled()
                .performClick()
            compose.waitUntil(10000) {
                active.get() &&
                    compose
                        .onAllNodesWithText("Configurer Owner service")
                        .fetchSemanticsNodes()
                        .isEmpty()
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Déconnecter", substring = false)
                    .fetchSemanticsNodes()
                    .size == 1
            }
            compose.onNodeWithText("Déconnecter", substring = false).performScrollTo()
            capture("owner-resource-authorization")
            compose
                .onNodeWithText("Déconnecter", substring = false)
                .performScrollTo()
                .performClick()
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(10000) { connected.isEmpty() }
            val disconnects = writes.filter {
                !it.second.containsKey("scopes") && it.first.endsWith("/oauth/owner")
            }
            assertEquals(
                "root1",
                disconnects.first().second["setupRevision"]?.jsonPrimitive?.content,
            )
            assertFalse(disconnects.last().second.containsKey("setupRevision"))
        }
    }

    @Test
    fun resourcePaginationCannotRestoreARevisionFromBeforeOAuth() {
        val manifest =
            PluginManifest(
                id = "resource-race",
                name = "Resource race",
                version = "1",
                resources =
                    listOf(
                        PluginResourceCollection(
                            name = "workspace",
                            title = "Workspaces",
                            singular = "Workspace",
                        )
                    ),
                oauth =
                    listOf(
                        PluginOAuthProvider(
                            name = "owner",
                            title = "Workspace authorization",
                            collection = "workspace",
                            scopes = listOf("messages:read"),
                        )
                    ),
            )
        val entry = PluginEntry(id = manifest.id, active = "1", manifest = manifest)
        val connection =
            PluginConnection(
                id = "app",
                pluginId = manifest.id,
                name = "Application",
                status = PluginStatus("ready"),
            )
        val revised = java.util.concurrent.atomic.AtomicBoolean(false)
        val heldPage = java.util.concurrent.CountDownLatch(1)
        val releasePage = java.util.concurrent.CountDownLatch(1)
        val refreshedStatus = java.util.concurrent.CountDownLatch(1)
        val savedRevision = java.util.concurrent.atomic.AtomicReference<String?>(null)
        fun resource(id: String) =
            PluginResource(
                id = id,
                connectionId = connection.id,
                collection = "workspace",
                name = if (id == "engineering") "Engineering" else "Sales",
                revision = if (id == "engineering" && revised.get()) "resource1" else "resource0",
            )
        fun status() =
            if (revised.get()) """{"status":"authorized","scopes":["messages:read"]}"""
            else """{"status":"disconnected"}"""
        MockWebServer().use { server ->
            val common =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("/plugins") ->
                                wireJson.encodeToString(
                                    PluginCatalog(
                                        packages = listOf(entry),
                                        connections = listOf(connection),
                                    )
                                )
                            path.endsWith("/resources/workspace") ->
                                wireJson.encodeToString(
                                    PluginPage(listOf(resource("engineering")), "engineering")
                                )
                            path.contains("/resources/workspace?cursor=") -> {
                                heldPage.countDown()
                                check(releasePage.await(30, java.util.concurrent.TimeUnit.SECONDS))
                                wireJson.encodeToString(PluginPage(listOf(resource("sales"))))
                            }
                            path.endsWith("/engineering/oauth/owner") -> {
                                if (revised.get()) refreshedStatus.countDown()
                                status()
                            }
                            path.endsWith("/sales/oauth/owner") -> """{"status":"disconnected"}"""
                            else -> null
                        }
                    }
                ) { path, _ ->
                    when {
                        path.endsWith("/oauth/owner/finish") -> {
                            revised.set(true)
                            status()
                        }
                        path.endsWith("/oauth/owner") ->
                            """{"state":"fixture-state","url":"https://provider.example.test/authorize","expiresAt":${System.currentTimeMillis() + 600000}}"""
                        else -> null
                    }
                }
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        if (
                            request.method == "PUT" &&
                                request.path.orEmpty().endsWith("/resources/workspace/engineering")
                        ) {
                            if (request.getHeader("X-CSRF-Token") != "fixture-csrf")
                                return MockResponse().setResponseCode(403)
                            val revision =
                                wireJson
                                    .parseToJsonElement(request.body.readUtf8())
                                    .jsonObject["revision"]
                                    ?.jsonPrimitive
                                    ?.content
                            savedRevision.set(revision)
                            return if (revision == "resource1")
                                MockResponse()
                                    .setBody(wireJson.encodeToString(resource("engineering")))
                            else
                                MockResponse()
                                    .setResponseCode(409)
                                    .setBody("""{"error":"Resource revision changed."}""")
                        }
                        return common.dispatch(request)
                    }
                }
            server.start()
            try {
                show(server) { vm, state -> PluginScreen(vm, state, manifest.id) {} }
                compose.waitUntil(30000) {
                    compose
                        .onAllNodesWithText("Connecter", substring = false)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("messages:read").performScrollTo().performClick()
                compose
                    .onNodeWithText("Connecter", substring = false)
                    .performScrollTo()
                    .performClick()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Vérifier l’autorisation")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Voir plus · Workspaces").performScrollTo().performClick()
                compose.waitUntil(10000) { heldPage.count == 0L }
                compose.onNodeWithText("Vérifier l’autorisation").performScrollTo().performClick()
                // This GET follows the real public refresh of resource1 while More is held.
                compose.waitUntil(10000) { refreshedStatus.count == 0L }
                releasePage.countDown()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodes(hasText("Modifier Engineering") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Modifier Engineering").performScrollTo().performClick()
                compose.onNodeWithText("Enregistrer", substring = false).performClick()
                compose.waitUntil(10000) { savedRevision.get() != null }
                assertEquals("resource1", savedRevision.get())
            } finally {
                releasePage.countDown()
            }
        }
    }

    @Test
    fun ownerProvidersShareOneResourceAuthorizationLock() {
        val manifest =
            PluginManifest(
                id = "owner-provider-lock",
                name = "Provider locks",
                version = "1",
                resources =
                    listOf(
                        PluginResourceCollection(
                            name = "workspace",
                            title = "Workspaces",
                            singular = "Workspace",
                        )
                    ),
                oauth =
                    listOf("owner" to "First authorization", "other" to "Second authorization")
                        .map { (name, title) ->
                            PluginOAuthProvider(
                                name = name,
                                title = title,
                                collection = "workspace",
                                scopes = listOf("messages:read"),
                            )
                        },
            )
        val entry = PluginEntry(id = manifest.id, active = "1", manifest = manifest)
        val connection =
            PluginConnection(
                id = "app",
                pluginId = manifest.id,
                name = "Application",
                status = PluginStatus("ready"),
            )
        val revision = java.util.concurrent.atomic.AtomicInteger(0)
        val connected = java.util.concurrent.ConcurrentHashMap.newKeySet<String>()
        val starts = CopyOnWriteArrayList<String>()
        fun resource() =
            PluginResource(
                id = "engineering",
                connectionId = connection.id,
                collection = "workspace",
                name = "Engineering",
                revision = "resource${revision.get()}",
            )
        fun status(provider: String) =
            if (provider in connected)
                """{"status":"authorized","scopes":["messages:read"],"requestedScopes":["messages:read"]}"""
            else """{"status":"disconnected"}"""
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("/plugins") ->
                                wireJson.encodeToString(
                                    PluginCatalog(
                                        packages = listOf(entry),
                                        connections = listOf(connection),
                                    )
                                )
                            path.endsWith("/resources/workspace") ->
                                wireJson.encodeToString(PluginPage(listOf(resource())))
                            "/oauth/" in path -> status(path.substringAfterLast('/'))
                            else -> null
                        }
                    }
                ) { path, body ->
                    if ("/oauth/" !in path) null
                    else {
                        val provider = path.substringAfter("/oauth/").substringBefore('/')
                        val input = wireJson.parseToJsonElement(body).jsonObject
                        if (path.endsWith("/finish")) {
                            connected.add(provider)
                            revision.incrementAndGet()
                            status(provider)
                        } else if ("scopes" in input) {
                            starts += provider
                            """{"state":"fixture-state-$provider","url":"https://provider.example.test/authorize","expiresAt":${System.currentTimeMillis() + 600000}}"""
                        } else {
                            connected.remove(provider)
                            revision.incrementAndGet()
                            status(provider)
                        }
                    }
                }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, manifest.id) {} }
            compose.waitUntil(30000) {
                compose
                    .onAllNodesWithText("Connecter", substring = false)
                    .fetchSemanticsNodes()
                    .size == 2
            }
            compose.onAllNodesWithText("messages:read")[0].performScrollTo().performClick()
            compose.onAllNodesWithText("messages:read")[1].performScrollTo().performClick()
            compose.onAllNodesWithText("Connecter", substring = false)[1].assertIsEnabled()
            compose
                .onAllNodesWithText("Connecter", substring = false)[0]
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Vérifier l’autorisation").fetchSemanticsNodes().size ==
                    1
            }
            compose.onNodeWithText("Connecter", substring = false).assertIsNotEnabled()
            assertEquals(listOf("owner"), starts.toList())
            compose.onNodeWithText("Vérifier l’autorisation").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("messages:read") and isEnabled())
                    .fetchSemanticsNodes()
                    .size == 2
            }
            compose.onAllNodesWithText("messages:read")[1].performScrollTo().performClick()
            compose
                .onNodeWithText("Connecter", substring = false)
                .assertIsEnabled()
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Vérifier l’autorisation").fetchSemanticsNodes().size ==
                    1
            }
            compose.onNodeWithText("Reconnecter", substring = false).assertIsNotEnabled()
            compose.onAllNodesWithText("Déconnecter", substring = false)[0].assertIsNotEnabled()
            compose
                .onAllNodesWithText("Déconnecter", substring = false)[1]
                .performScrollTo()
                .performClick()
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("messages:read") and isEnabled())
                    .fetchSemanticsNodes()
                    .size == 2
            }
            compose.onNodeWithText("Connecter", substring = false).assertExists()
            compose.onNodeWithText("Connecté", substring = false).assertExists()
            assertEquals(listOf("owner", "other"), starts.toList())
        }
    }

    @Test
    fun resourceAccessUsesExplicitProfilesOverridesAndScopeLimits() {
        val manifest =
            PluginManifest(
                id = "access-ui",
                name = "Resource access",
                version = "1",
                agentAccess = PluginAgentAccess(),
                resources =
                    listOf(PluginResourceCollection("workspace", "Workspaces", "Workspace")),
                operations =
                    listOf(
                        PluginOperation(
                            "read-messages",
                            "workspace",
                            PluginAccessProfile.Read,
                            listOf("channel"),
                        ),
                        PluginOperation(
                            "send-message",
                            "workspace",
                            PluginAccessProfile.Contribute,
                            listOf("channel"),
                        ),
                        PluginOperation(
                            "delete-message",
                            "workspace",
                            PluginAccessProfile.Manage,
                            listOf("channel"),
                        ),
                        PluginOperation("list-people", "workspace", PluginAccessProfile.Read),
                    ),
            )
        val entry = PluginEntry(id = manifest.id, active = "1", manifest = manifest)
        val resource =
            PluginResource(
                id = "engineering",
                connectionId = "app",
                collection = "workspace",
                name = "Engineering",
                revision = "r1",
            )
        val rules = CopyOnWriteArrayList<PluginResourceGrant>()
        val writes = CopyOnWriteArrayList<JsonObject>()
        val uncertain = java.util.concurrent.atomic.AtomicBoolean(false)
        val grantsStarted = CountDownLatch(1)
        val releaseGrants = CountDownLatch(1)
        MockWebServer().use { server ->
            val normal =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("/plugins") ->
                                wireJson.encodeToString(
                                    PluginCatalog(
                                        packages = listOf(entry),
                                        connections =
                                            listOf(
                                                PluginConnection(
                                                    id = "app",
                                                    pluginId = manifest.id,
                                                    name = "Application",
                                                    status = PluginStatus("ready"),
                                                )
                                            ),
                                    )
                                )
                            path.endsWith("/resources/workspace") ->
                                wireJson.encodeToString(PluginPage(listOf(resource)))
                            path.endsWith("/grants") ->
                                wireJson.encodeToString(PluginPage(rules.toList()))
                            else -> null
                        }
                    }
                ) { path, body ->
                    when {
                        path.endsWith("/grants") -> {
                            val input = wireJson.parseToJsonElement(body).jsonObject
                            writes += input
                            val grant =
                                wireJson
                                    .decodeFromString<PluginResourceGrant>(body)
                                    .copy(id = "grant${writes.size}", resourceId = resource.id)
                            rules += grant
                            wireJson.encodeToString(grant)
                        }
                        path.contains("/grants/") -> {
                            rules.removeAll { it.id == path.substringAfterLast('/') }
                            "{}"
                        }
                        else -> null
                    }
                }
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val response = normal.dispatch(request)
                        if (request.method == "GET" && request.path.orEmpty().endsWith("/grants")) {
                            grantsStarted.countDown()
                            check(releaseGrants.await(20, TimeUnit.SECONDS))
                        }
                        return if (
                            uncertain.get() &&
                                request.method != "GET" &&
                                request.path.orEmpty().contains("/grants")
                        )
                            MockResponse().setSocketPolicy(SocketPolicy.DISCONNECT_AFTER_REQUEST)
                        else response
                    }
                }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, manifest.id) {} }
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Accès des agents").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Accès des agents").performScrollTo().performClick()
            try {
                compose.waitUntil(10000) { grantsStarted.count == 0L }
                compose.onNodeWithText("Chargement des règles…").assertExists()
                compose.onNodeWithText("Ajouter une règle").assertDoesNotExist()
                compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
                compose.onNodeWithContentDescription("Fermer").performClick()
                compose.onNodeWithText("Accès des agents · Engineering").assertDoesNotExist()
            } finally {
                releaseGrants.countDown()
            }
            compose.onNodeWithText("Accès des agents").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Aucune règle d’accès.")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Aucune règle d’accès.").assertExists()
            assertTrue(writes.isEmpty())
            compose.onNodeWithText("Ajouter une règle").performScrollTo().performClick()
            compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
            compose.onNodeWithContentDescription("Helper").performScrollTo().performClick()
            compose.onNodeWithContentDescription("Contribution").performScrollTo().performClick()
            compose
                .onNodeWithContentDescription("Refuser read-messages")
                .performScrollTo()
                .performClick()
            compose
                .onNodeWithContentDescription("Autoriser delete-message")
                .performScrollTo()
                .performClick()
            compose
                .onNodeWithText("Limiter channel")
                .performScrollTo()
                .performTextInput("C111\nC222")
            compose
                .onNodeWithText("Autorisées par cette règle : send-message, delete-message")
                .performScrollTo()
                .assertExists()
            compose.onNodeWithText("Enregistrer").assertIsEnabled().performClick()
            compose.waitUntil(10000) {
                rules.size == 1 &&
                    compose
                        .onAllNodesWithText("Ajouter une règle")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            val input = writes.single()
            assertEquals("helper", input["agentId"]?.jsonPrimitive?.content)
            assertEquals("contribute", input["profile"]?.jsonPrimitive?.content)
            assertEquals(
                listOf("delete-message"),
                input["operations"]?.jsonArray?.map { it.jsonPrimitive.content },
            )
            assertEquals(
                listOf("read-messages"),
                input["deniedOperations"]?.jsonArray?.map { it.jsonPrimitive.content },
            )
            assertEquals(
                listOf("C111", "C222"),
                input["scopes"]?.jsonObject?.get("channel")?.jsonArray?.map {
                    it.jsonPrimitive.content
                },
            )
            compose.onNodeWithText("channel : C111, C222").performScrollTo().assertExists()
            capture("resource-access-rules")
            compose.onNodeWithContentDescription("Fermer").performClick()
            compose
                .onNodeWithText("Accès global à l’instance : aucun agent.")
                .performScrollTo()
                .assertExists()
            compose
                .onNodeWithText("Aucun agent ne peut l’utiliser pour l’instant.")
                .assertDoesNotExist()
            compose.onNodeWithText("Accès des agents").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Révoquer la règle").fetchSemanticsNodes().isNotEmpty()
            }

            compose.onNodeWithText("Révoquer la règle").performScrollTo().performClick()
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(10000) {
                rules.isEmpty() &&
                    compose
                        .onAllNodesWithText("Aucune règle d’accès.")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            assertEquals(1, writes.size)
            uncertain.set(true)
            compose.onNodeWithText("Ajouter une règle").performScrollTo().performClick()
            compose.onNodeWithContentDescription("Helper").performScrollTo().performClick()
            compose.onNodeWithContentDescription("Lecture").performScrollTo().performClick()
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) {
                rules.size == 1 &&
                    compose
                        .onAllNodesWithText(
                            "Actualisez les règles avant une autre modification.",
                            substring = true,
                        )
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            compose.onNodeWithText("Ajouter une règle").assertDoesNotExist()
            assertEquals(2, writes.size)
            compose.onNodeWithText("Actualiser les règles").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Ajouter une règle").fetchSemanticsNodes().isNotEmpty()
            }
            assertEquals(2, writes.size)
            compose.onNodeWithText("Révoquer la règle").performScrollTo().performClick()
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(10000) {
                rules.isEmpty() &&
                    compose
                        .onAllNodesWithText(
                            "Actualisez les règles avant une autre modification.",
                            substring = true,
                        )
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            compose.onNodeWithText("Actualiser les règles").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Aucune règle d’accès.")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            assertEquals(2, writes.size)
        }
    }

    @Test
    fun inertResourcesDoNotOfferOperationalGrants() {
        val manifest =
            PluginManifest(
                id = "draft-grants",
                agentAccess = PluginAgentAccess(),
                resources =
                    listOf(
                        PluginResourceCollection(
                            "workspace",
                            "Workspaces",
                            "Workspace",
                            delegation = true,
                        )
                    ),
                operations = listOf(PluginOperation("read", "workspace", PluginAccessProfile.Read)),
            )
        val resource =
            PluginResource(
                id = "engineering",
                connectionId = "draft",
                collection = "workspace",
                name = "Prepared workspace",
                revision = "r1",
            )
        val grantReads = java.util.concurrent.atomic.AtomicInteger()
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("/resources/workspace") ->
                                wireJson.encodeToString(PluginPage(listOf(resource)))
                            path.endsWith("/grants") -> {
                                grantReads.incrementAndGet()
                                "{\"items\":[]}"
                            }
                            else -> null
                        }
                    }
                ) { _, _ ->
                    null
                }
            server.start()
            show(server) { vm, state ->
                Column {
                    PluginResources(
                        vm,
                        state,
                        manifest,
                        PluginResourceLocation(manifest.id, "draft", "root1"),
                    )
                }
            }
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Prepared workspace").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Accès des agents").assertDoesNotExist()
            compose.onNodeWithText("Routes de conversation").assertDoesNotExist()
            assertEquals(0, grantReads.get())
        }
    }

    @Test
    fun resourceRoutesLoadAllPagesAndCancelAClosedRead() {
        resourceManagementPagesScenario(routes = true)
    }

    @Test
    fun resourceGrantsLoadAllPagesAndCancelAClosedRead() {
        resourceManagementPagesScenario(routes = false)
    }

    private fun resourceManagementPagesScenario(routes: Boolean) {
        val root = "/plugins/pages/connections/app/resources/workspace/engineering"
        val path = "$root/${if (routes) "routes" else "grants"}"
        val items = CopyOnWriteArrayList<JsonElement>()
        for (id in listOf("first", "last")) {
            items +=
                if (routes)
                    wireJson.encodeToJsonElement(
                        PluginRoute(
                            id = id,
                            resourceId = "engineering",
                            name = id,
                            agentId = "helper",
                            userIds = listOf("alice"),
                            revision = "revision-$id",
                        )
                    )
                else
                    wireJson.encodeToJsonElement(
                        PluginResourceGrant(
                            id = id,
                            resourceId = "engineering",
                            agentId = "helper",
                            profile = PluginAccessProfile.Read,
                            scopes = mapOf("channel" to listOf(id)),
                        )
                    )
        }
        val started = CountDownLatch(1)
        val release = CountDownLatch(1)
        val hold = java.util.concurrent.atomic.AtomicBoolean(true)
        val deleted = CopyOnWriteArrayList<String>()
        val operations =
            listOf(
                PluginOperation(
                    "read-messages",
                    "workspace",
                    PluginAccessProfile.Read,
                    listOf("channel"),
                )
            )
        MockWebServer().use { server ->
            val normal = dispatcher { _, _ -> null }
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val requested = request.requestUrl!!
                        if (request.method == "GET" && requested.encodedPath.endsWith(path)) {
                            val cursor = requested.queryParameter("cursor")
                            val remaining = items.filter {
                                cursor == null ||
                                    it.jsonObject["id"]!!.jsonPrimitive.content > cursor
                            }
                            if (cursor != null && hold.get()) {
                                started.countDown()
                                check(release.await(20, TimeUnit.SECONDS))
                            }
                            val page = buildJsonObject {
                                put("items", JsonArray(remaining.take(1)))
                                put(
                                    "nextCursor",
                                    if (remaining.size > 1) remaining.first().jsonObject["id"]!!
                                    else JsonNull,
                                )
                            }
                            return MockResponse().setBody(page.toString())
                        }
                        if (
                            request.method == "DELETE" && requested.encodedPath.contains("$path/")
                        ) {
                            assertEquals("fixture-csrf", request.getHeader("X-CSRF-Token"))
                            val id = requested.pathSegments.last()
                            if (routes)
                                assertEquals(
                                    "revision-$id",
                                    wireJson
                                        .parseToJsonElement(request.body.readUtf8())
                                        .jsonObject["revision"]!!
                                        .jsonPrimitive
                                        .content,
                                )
                            deleted += id
                            items.removeAll { it.jsonObject["id"]!!.jsonPrimitive.content == id }
                            return MockResponse().setBody("{}")
                        }
                        return normal.dispatch(request)
                    }
                }
            server.start()
            show(server) { vm, state ->
                if (routes) PluginResourceRoutes(vm, state, root, "Engineering", true) {}
                else PluginResourceGrants(vm, state, path, "Engineering", operations, true) {}
            }
            val action = if (routes) "Routes de conversation" else "Accès des agents"
            val loading = if (routes) "Chargement des routes…" else "Chargement des règles…"
            val add = if (routes) "Ajouter une route" else "Ajouter une règle"
            compose.waitUntil(10000) {
                compose.onAllNodesWithText(action).fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText(action).performClick()
            try {
                compose.waitUntil(10000) { started.count == 0L }
                compose.onNodeWithText(loading).assertExists()
                compose.onNodeWithText(add).assertDoesNotExist()
                compose.onNodeWithContentDescription("Fermer").performClick()
                compose.onNodeWithText("$action · Engineering").assertDoesNotExist()
            } finally {
                hold.set(false)
                release.countDown()
            }
            compose.onNodeWithText(action).performClick()
            val last = if (routes) "Supprimer la route last" else "channel : last"
            compose.waitUntil(10000) {
                compose.onAllNodesWithText(last).fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText(last).performScrollTo().assertExists()
            if (routes) {
                compose.onNodeWithText(last).performClick()
                compose.onNodeWithText("Confirmer la suppression").performScrollTo().performClick()
            } else {
                compose.onAllNodesWithText("Révoquer la règle")[1].performScrollTo().performClick()
                compose.onNodeWithText("Confirmer").performClick()
            }
            compose.waitUntil(10000) {
                deleted.size == 1 &&
                    compose.onAllNodesWithText(last).fetchSemanticsNodes().isEmpty() &&
                    compose.onAllNodesWithText(add).fetchSemanticsNodes().isNotEmpty()
            }
            assertEquals(listOf("last"), deleted.toList())
            compose.onNodeWithText(add).performScrollTo().assertExists()
        }
    }

    @Test
    fun resourceRoutesRequireExplicitSourceAndAgentAndPreservePausedConditions() {
        resourceRouteEditorScenario(uncertain = false)
    }

    @Test
    fun resourceRoutesRefreshAnUncertainCreationWithoutRepeatingIt() {
        resourceRouteEditorScenario(uncertain = true)
    }

    private fun resourceRouteEditorScenario(uncertain: Boolean) {
        val routes = CopyOnWriteArrayList<PluginRoute>()
        val writes = CopyOnWriteArrayList<JsonObject>()
        val methods = CopyOnWriteArrayList<String>()
        val eligibilityReads = AtomicInteger()
        val editPreviewStarted = CountDownLatch(1)
        val releaseEditPreview = CountDownLatch(1)
        val routePath = "/plugins/routes-ui/connections/app/resources/workspace/engineering"
        val path = "$routePath/routes"
        MockWebServer().use { server ->
            val normal =
                dispatcher(
                    read = { requested ->
                        when {
                            requested.endsWith(path) ->
                                wireJson.encodeToString(PluginPage(routes.toList()))
                            requested.contains("$routePath/route-eligibility?") ->
                                wireJson.encodeToString(
                                    PluginPage(listOf(RouteEligibility("helper", true)))
                                )
                            else -> null
                        }
                    }
                ) { _, _ ->
                    null
                }
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val requested = request.path.orEmpty()
                        if (
                            request.method == "GET" &&
                                requested.contains("$routePath/route-eligibility?") &&
                                eligibilityReads.incrementAndGet() == 2
                        ) {
                            editPreviewStarted.countDown()
                            check(releaseEditPreview.await(20, TimeUnit.SECONDS))
                        }
                        if (request.method == "GET" || !requested.contains(path))
                            return normal.dispatch(request)
                        if (request.getHeader("X-CSRF-Token") != "fixture-csrf")
                            return MockResponse().setResponseCode(403)
                        val input = wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                        methods += request.method.orEmpty()
                        writes += input
                        if (request.method == "DELETE") {
                            assertEquals(
                                routes.single().revision,
                                input["revision"]?.jsonPrimitive?.content,
                            )
                            routes.clear()
                            return MockResponse().setBody("{}")
                        }
                        if (request.method == "PUT")
                            assertEquals(
                                routes.single().revision,
                                input["revision"]?.jsonPrimitive?.content,
                            )
                        val saved =
                            wireJson
                                .decodeFromString<PluginRoute>(input.toString())
                                .copy(
                                    id = "route",
                                    resourceId = "engineering",
                                    revision = "edit${writes.size}",
                                )
                        routes.clear()
                        routes += saved
                        return if (uncertain)
                            MockResponse().setSocketPolicy(SocketPolicy.DISCONNECT_AFTER_REQUEST)
                        else MockResponse().setBody(wireJson.encodeToString(saved))
                    }
                }
            server.start()
            show(server) { vm, state ->
                PluginResourceRoutes(vm, state, routePath, "Engineering", true) {}
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Routes de conversation")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Routes de conversation").performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Ajouter une route").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Ajouter une route").performScrollTo().performClick()
            compose.onNodeWithText("Nom de la route").performScrollTo().performTextInput("Support")
            compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
            compose.waitUntil(10000) {
                compose.onAllNodesWithContentDescription("Helper").fetchSemanticsNodes().any {
                    !it.config.contains(SemanticsProperties.Disabled)
                }
            }
            compose.onNodeWithContentDescription("Helper").performScrollTo().performClick()
            compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
            compose.onNodeWithText("Ajouter une condition").performScrollTo().performClick()
            compose
                .onNodeWithText("Clé de la condition 1")
                .performScrollTo()
                .performTextInput("constructor")
            compose
                .onNodeWithText("Valeur de la condition 1")
                .performScrollTo()
                .performTextInput("C1")
            compose.onNodeWithText("Enregistrer").assertIsEnabled().performClick()
            compose.waitUntil(10000) {
                routes.size == 1 &&
                    compose
                        .onAllNodesWithText(
                            if (uncertain) "Actualisez les routes avant une autre modification."
                            else "Modifier la route Support",
                            substring = true,
                        )
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            assertEquals(1, writes.size)
            assertEquals("helper", writes.single()["agentId"]?.jsonPrimitive?.content)
            assertEquals("C1", routes.single().match["constructor"])
            assertEquals("requests", routes.single().accessMode)
            assertTrue(routes.single().userIds.isEmpty())
            if (uncertain) {
                compose.onNodeWithText("Ajouter une route").assertDoesNotExist()
                compose.onNodeWithText("Actualiser les routes").performScrollTo().performClick()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Modifier la route Support")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                assertEquals(1, writes.size)
                capture("resource-route-uncertain-refreshed")
            } else {
                capture("resource-conversation-route")
                compose.onNodeWithText("Modifier la route Support").performScrollTo().performClick()
                compose.onNodeWithText("Valeur de la condition 1").assertTextContains("C1")
                try {
                    compose.waitUntil(10000) { editPreviewStarted.count == 0L }
                    compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
                    compose.onNodeWithText("Route activée").performScrollTo().performClick()
                    compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
                } finally {
                    releaseEditPreview.countDown()
                }
                compose.waitUntil(10000) {
                    compose.onAllNodesWithText("Enregistrer").fetchSemanticsNodes().any {
                        !it.config.contains(SemanticsProperties.Disabled)
                    }
                }
                compose.onNodeWithText("Enregistrer").assertIsEnabled().performClick()
                compose.waitUntil(10000) {
                    methods.size == 2 &&
                        compose
                            .onAllNodesWithText("Support · En pause")
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                }
                assertFalse(routes.single().enabled)
                assertEquals(mapOf("constructor" to "C1"), routes.single().match)
                compose
                    .onNodeWithText("Supprimer la route Support")
                    .performScrollTo()
                    .performClick()
                compose
                    .onNodeWithText(
                        "Supprimer Support ? Une route plus générale pourra alors correspondre à ses sources."
                    )
                    .performScrollTo()
                    .assertExists()
                compose.onNodeWithText("Confirmer la suppression").performScrollTo().performClick()
                compose.waitUntil(10000) {
                    routes.isEmpty() &&
                        compose
                            .onAllNodesWithText("Ajouter une route")
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                }
                assertEquals(listOf("POST", "PUT", "DELETE"), methods.toList())
            }
        }
    }

    @Test
    fun resourceRoutePeopleUseTheirOwnRevisionAndNeverRetryAStaleAction() {
        val root = "/plugins/routes-people/connections/app/resources/workspace/engineering"
        val routes =
            CopyOnWriteArrayList(
                listOf(
                    PluginRoute(
                        id = "support",
                        resourceId = "engineering",
                        name = "Support",
                        agentId = "helper",
                        accessMode = "requests",
                        match = mapOf("channel" to "C1"),
                        revision = "s1",
                    ),
                    PluginRoute(
                        id = "operations",
                        resourceId = "engineering",
                        name = "Operations",
                        agentId = "helper",
                        accessMode = "requests",
                        match = mapOf("channel" to "C2"),
                        revision = "o1",
                    ),
                )
            )
        val requests =
            mapOf(
                "support" to
                    CopyOnWriteArrayList(
                        listOf(PluginRouteRequest("q1", "U1", "Person U1", "pending"))
                    ),
                "operations" to
                    CopyOnWriteArrayList(
                        listOf(PluginRouteRequest("q2", "U2", "Person U2", "pending"))
                    ),
            )
        val writes = CopyOnWriteArrayList<Pair<String, JsonObject>>()
        MockWebServer().use { server ->
            val normal =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("$root/routes") ->
                                wireJson.encodeToString(PluginPage(routes.toList()))
                            path.contains("$root/routes/") && path.endsWith("/requests") -> {
                                val id = path.substringAfter("$root/routes/").substringBefore('/')
                                wireJson.encodeToString(PluginPage(requests.getValue(id).toList()))
                            }
                            else -> null
                        }
                    }
                ) { _, _ ->
                    null
                }
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path.orEmpty()
                        if (
                            request.method != "POST" ||
                                !path.contains("$root/routes/") ||
                                !path.contains("/people/")
                        )
                            return normal.dispatch(request)
                        if (request.getHeader("X-CSRF-Token") != "fixture-csrf")
                            return MockResponse().setResponseCode(403)
                        val input = wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                        writes += path to input
                        val id = path.substringAfter("$root/routes/").substringBefore('/')
                        val person = path.substringAfterLast('/')
                        val index = routes.indexOfFirst { it.id == id }
                        val current = routes[index]
                        if (input["revision"]?.jsonPrimitive?.content != current.revision)
                            return MockResponse()
                                .setResponseCode(409)
                                .setBody("""{"error":"The route changed."}""")
                        val action = input["action"]?.jsonPrimitive?.content
                        val users =
                            when (action) {
                                "approve" -> (current.userIds + person).distinct()
                                "remove" -> current.userIds - person
                                else -> current.userIds
                            }
                        if (action == "deny") {
                            val pending = requests.getValue(id)
                            val requestIndex = pending.indexOfFirst { it.userId == person }
                            pending[requestIndex] = pending[requestIndex].copy(status = "denied")
                        } else requests.getValue(id).removeAll { it.userId == person }
                        val saved =
                            current.copy(
                                userIds = users,
                                people = users.map { PluginLabel(it, "Person $it") },
                                revision = "r${writes.size}",
                            )
                        routes[index] = saved
                        return MockResponse().setBody(wireJson.encodeToString(saved))
                    }
                }
            server.start()
            show(server) { vm, state ->
                PluginResourceRoutes(vm, state, root, "Engineering", true) {}
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Routes de conversation")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Routes de conversation").performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Personnes et demandes · Support")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose
                .onNodeWithText("Personnes et demandes · Support")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Approuver U1").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Person U2").assertDoesNotExist()
            capture("resource-route-people")
            compose.onNodeWithText("Approuver U1").performScrollTo().performClick()
            compose.waitUntil(10000) {
                writes.size == 1 &&
                    compose
                        .onAllNodesWithText("Personnes et demandes · Support")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            assertEquals(listOf("U1"), routes.first().userIds)
            assertTrue(requests.getValue("support").isEmpty())
            assertEquals("pending", requests.getValue("operations").single().status)
            assertTrue(writes.single().first.endsWith("/routes/support/people/U1"))
            assertEquals("s1", writes.single().second["revision"]?.jsonPrimitive?.content)
            compose
                .onNodeWithText("Personnes et demandes · Support")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Retirer U1").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Retirer U1").performScrollTo().performClick()
            compose.waitUntil(10000) {
                writes.size == 2 &&
                    compose
                        .onAllNodesWithText("Personnes et demandes · Operations")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            assertTrue(routes.first().userIds.isEmpty())
            compose
                .onNodeWithText("Personnes et demandes · Operations")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Refuser U2").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Refuser U2").performScrollTo().performClick()
            compose.waitUntil(10000) {
                writes.size == 3 &&
                    compose
                        .onAllNodesWithText("Personnes et demandes · Support")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            assertEquals("denied", requests.getValue("operations").single().status)
            assertTrue(writes.last().first.endsWith("/routes/operations/people/U2"))
            requests.getValue("support") += PluginRouteRequest("q3", "U3", "Person U3", "pending")
            compose
                .onNodeWithText("Personnes et demandes · Support")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Approuver U3").fetchSemanticsNodes().isNotEmpty()
            }
            routes[0] = routes[0].copy(revision = "external-change")
            compose.onNodeWithText("Approuver U3").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText(
                        "Actualisez les routes avant une autre modification.",
                        substring = true,
                    )
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            assertEquals(4, writes.size)
            assertTrue(routes.first().userIds.isEmpty())
            assertEquals("pending", requests.getValue("support").single().status)
            compose.onNodeWithText("Approuver U3").assertDoesNotExist()
            compose.onNodeWithText("Actualiser les routes").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Personnes et demandes · Support")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose
                .onNodeWithText("Personnes et demandes · Support")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Approuver U3").fetchSemanticsNodes().isNotEmpty()
            }
            assertEquals(4, writes.size)
            compose.onNodeWithText("Approuver U3").performScrollTo().performClick()
            compose.waitUntil(10000) {
                writes.size == 5 &&
                    compose
                        .onAllNodesWithText("Personnes et demandes · Support")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            assertEquals(listOf("U3"), routes.first().userIds)
            assertEquals(
                "external-change",
                writes.last().second["revision"]?.jsonPrimitive?.content,
            )
        }
    }

    @Test
    fun routeRequestsDoNotUseInstanceApprovalActions() {
        val manifest =
            PluginManifest(
                id = "route-inbox",
                name = "Route inbox",
                version = "1",
                capabilities = listOf("conversations.submit"),
                resources =
                    listOf(
                        PluginResourceCollection(
                            name = "workspace",
                            title = "Workspaces",
                            singular = "Workspace",
                            delegation = true,
                        )
                    ),
            )
        val catalog =
            wireJson
                .encodeToJsonElement(
                    PluginCatalog(
                        packages =
                            listOf(
                                PluginEntry(id = manifest.id, active = "1", manifest = manifest)
                            ),
                        connections =
                            listOf(
                                PluginConnection(
                                    id = "app",
                                    pluginId = manifest.id,
                                    name = "Application",
                                    status = PluginStatus("ready"),
                                    requests =
                                        listOf(
                                            PluginRequest(
                                                id = "request",
                                                userId = "pending",
                                                name = "Route participant",
                                            )
                                        ),
                                )
                            ),
                    )
                )
                .jsonObject
        val connection = catalog.getValue("connections").jsonArray.single().jsonObject
        val request = connection.getValue("requests").jsonArray.single().jsonObject
        val routed =
            JsonObject(
                catalog +
                    ("connections" to
                        JsonArray(
                            listOf(
                                JsonObject(
                                    connection +
                                        ("requests" to
                                            JsonArray(
                                                listOf(
                                                    JsonObject(
                                                        request +
                                                            ("routeId" to JsonPrimitive("route1"))
                                                    )
                                                )
                                            ))
                                )
                            )
                        ))
            )
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("/plugins") -> routed.toString()
                            path.endsWith("/resources/workspace") -> "{\"items\":[]}"
                            else -> null
                        }
                    }
                ) { _, _ ->
                    null
                }
            server.start()
            show(server) { vm, state -> PluginScreen(vm, state, manifest.id) {} }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Application", substring = false)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Route participant").assertDoesNotExist()
            compose.onNodeWithText("Approuver", substring = false).assertDoesNotExist()
            compose.onNodeWithText("Refuser", substring = false).assertDoesNotExist()
        }
    }

    @Test
    fun largeGrantInputsUseSmallSavedStateAndRetainExactLimits() = grantInputRestoration(false)

    @Test fun missingLocalGrantInputsCannotRestoreAnUnrestrictedRule() = grantInputRestoration(true)

    private fun grantInputRestoration(dropLocalInputs: Boolean) {
        val keys = (0 until 8).map { "scope$it" }
        val operations =
            listOf(PluginOperation("read", "workspace", PluginAccessProfile.Read, keys))
        val expected =
            keys
                .mapIndexed { n, key ->
                    key to
                        (0 until 100).map { i ->
                            "$n-${i.toString().padStart(3, '0')}-${"x".repeat(172)}"
                        }
                }
                .toMap()
        val path = "/plugins/grant-draft/connections/app/resources/workspace/engineering/grants"
        val writes = CopyOnWriteArrayList<JsonObject>()
        val restoration = StateRestorationTester(compose)
        val registry = AtomicReference<androidx.compose.runtime.saveable.SaveableStateRegistry>()
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { requested ->
                        if (requested.endsWith(path)) """{"items":[]}""" else null
                    }
                ) { requested, body ->
                    if (requested.endsWith(path)) {
                        writes += wireJson.parseToJsonElement(body).jsonObject
                        "{}"
                    } else null
                }
            server.start()
            val vm =
                show(server, restoration) { model, state ->
                    val current =
                        androidx.compose.runtime.saveable.LocalSaveableStateRegistry.current
                    SideEffect { registry.set(checkNotNull(current)) }
                    PluginResourceGrants(model, state, path, "Engineering", operations, true) {}
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            fun waitEnabled(label: String) =
                compose.waitUntil(10000) {
                    compose
                        .onAllNodes(hasText(label) and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
            try {
                waitEnabled("Accès des agents")
                compose.onNodeWithText("Accès des agents").performClick()
                waitEnabled("Ajouter une règle")
                compose.onNodeWithText("Ajouter une règle").performScrollTo().performClick()
                compose.onNodeWithContentDescription("Helper").performScrollTo().performClick()
                compose.onNodeWithContentDescription("Lecture").performScrollTo().performClick()
                for (key in keys) {
                    compose
                        .onNodeWithText("Limiter $key")
                        .performScrollTo()
                        .performTextReplacement(expected.getValue(key).joinToString("\n"))
                }
                waitEnabled("Enregistrer")
                val bytes = compose.runOnIdle {
                    val parcel = android.os.Parcel.obtain()
                    try {
                        parcel.writeValue(registry.get().performSave())
                        parcel.dataSize()
                    } finally {
                        parcel.recycle()
                    }
                }
                assertTrue(
                    "Grant inputs must not inflate activity state: $bytes bytes",
                    bytes < 65536,
                )
                println("Grant input activity state: $bytes bytes")
                if (dropLocalInputs)
                    PluginSettingsDraftStore.clear(ApplicationProvider.getApplicationContext())
                restoration.emulateSavedInstanceStateRestore()
                if (dropLocalInputs) {
                    waitEnabled("Ajouter une règle")
                    compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
                    compose
                        .onNodeWithText(
                            "Les limites saisies n’ont pas pu être récupérées. Recommencez la règle et vérifiez ses périmètres."
                        )
                        .assertExists()
                    assertTrue(
                        "Missing local limits must not submit a broader rule",
                        writes.isEmpty(),
                    )
                    compose.onNodeWithText("Ajouter une règle").performScrollTo().performClick()
                    assertEquals(
                        "",
                        compose
                            .onNodeWithText("Limiter scope0")
                            .fetchSemanticsNode()
                            .config[SemanticsProperties.EditableText]
                            .text,
                    )
                    compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
                    capture("lost-grant-input-requires-new-rule")
                    return
                }
                waitEnabled("Enregistrer")
                for (key in listOf(keys.first(), keys.last())) {
                    compose
                        .onNodeWithText("Limiter $key")
                        .performScrollTo()
                        .assertTextContains(expected.getValue(key).joinToString("\n"))
                }
                assertTrue("Restoration must not submit a rule", writes.isEmpty())
                compose.onNodeWithText("Enregistrer").performClick()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodes(hasText("Ajouter une règle") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty() && writes.size == 1
                }
                assertTrue(
                    "This is an allowed native request size",
                    writes.single().toString().toByteArray().size < 150000,
                )
                assertEquals(wireJson.encodeToJsonElement(expected), writes.single()["scopes"])
                compose.onNodeWithText("Ajouter une règle").performScrollTo().performClick()
                assertEquals(
                    "",
                    compose
                        .onNodeWithText("Limiter scope0")
                        .fetchSemanticsNode()
                        .config[SemanticsProperties.EditableText]
                        .text,
                )
                compose
                    .onNodeWithText("Limiter scope0")
                    .performScrollTo()
                    .performTextReplacement("cancelled-scope")
                compose.onNodeWithText("Annuler la règle").performScrollTo().performClick()
                compose.onNodeWithText("Ajouter une règle").performScrollTo().performClick()
                assertEquals(
                    "",
                    compose
                        .onNodeWithText("Limiter scope0")
                        .fetchSemanticsNode()
                        .config[SemanticsProperties.EditableText]
                        .text,
                )
                capture("large-grant-input-restored")
            } finally {
                models.clear()
            }
        }
    }

    @Test
    fun largeUnsavedResourceInputsUseSmallSavedStateAndNeverPersistSecrets() {
        val fields =
            (0 until 29).map { n ->
                PluginSetting(key = "field$n", title = "Field $n", maxLength = 10000)
            } + PluginSetting(key = "secret", title = "Credential", type = "secret")
        val manifest =
            PluginManifest(
                id = "large-resource",
                name = "Large resource",
                version = "1",
                resources =
                    listOf(
                        PluginResourceCollection(
                            name = "workspace",
                            title = "Workspaces",
                            singular = "Workspace",
                            settings = fields,
                        )
                    ),
            )
        val restoration = StateRestorationTester(compose)
        val registry = AtomicReference<androidx.compose.runtime.saveable.SaveableStateRegistry>()
        val writes = CopyOnWriteArrayList<String>()
        val input = "\"".repeat(10000)
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        if (path.endsWith("/resources/workspace")) "{\"items\":[]}" else null
                    }
                ) { path, _ ->
                    writes += path
                    "{}"
                }
            server.start()
            val vm =
                show(server, restoration) { model, state ->
                    val current =
                        androidx.compose.runtime.saveable.LocalSaveableStateRegistry.current
                    SideEffect { registry.set(checkNotNull(current)) }
                    PluginResources(
                        model,
                        state,
                        manifest,
                        PluginResourceLocation(manifest.id, "app"),
                    )
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                compose.waitUntil(10000) {
                    compose
                        .onAllNodes(hasText("Ajouter · Workspace") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Ajouter · Workspace").performClick()
                compose
                    .onNodeWithText("Nom · Workspace")
                    .performTextReplacement("Ordinary resource draft")
                for (n in 0 until 29) {
                    compose
                        .onNodeWithText("Field $n", substring = false)
                        .performScrollTo()
                        .performTextReplacement(input)
                }
                compose
                    .onNodeWithText("Credential")
                    .performScrollTo()
                    .performTextInput("resource-fixture-secret")
                val bytes = compose.runOnIdle {
                    val parcel = android.os.Parcel.obtain()
                    try {
                        val saved = registry.get().performSave()
                        assertFalse(saved.toString().contains("resource-fixture-secret"))
                        parcel.writeValue(saved)
                        parcel.dataSize()
                    } finally {
                        parcel.recycle()
                    }
                }
                assertTrue(
                    "Unsaved resource inputs must not inflate activity state: $bytes bytes",
                    bytes < 65_536,
                )
                println("Unsaved resource activity state: $bytes bytes")
                restoration.emulateSavedInstanceStateRestore()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodes(hasText("Field 0") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                for (n in listOf(0, 28)) {
                    compose
                        .onNodeWithText("Field $n", substring = false)
                        .performScrollTo()
                        .assertTextContains(input)
                }
                assertEquals(
                    "",
                    compose
                        .onNodeWithText("Credential")
                        .performScrollTo()
                        .fetchSemanticsNode()
                        .config[SemanticsProperties.EditableText]
                        .text,
                )
                assertTrue(writes.isEmpty())
            } finally {
                models.clear()
            }
        }
    }

    @Test
    fun largeSetupSavesOnlySmallEditorReferencesAndOrdinaryEdits() {
        val fields =
            (0 until 20).map { n ->
                PluginSetting(
                    key = "field$n",
                    title = "Field $n",
                    maxLength = 10000,
                    default = if (n < 6) JsonPrimitive("\"".repeat(10000)) else null,
                )
            } + PluginSetting(key = "secret", title = "Credential", type = "secret")
        val manifest =
            PluginManifest(id = "large-restore", name = "Large restore", settings = fields)
        val values =
            JsonObject(
                (0 until 20).associate {
                    "field$it" to
                        JsonPrimitive(if (it < 6) "\"".repeat(10000) else "x".repeat(10000))
                }
            )
        val draft =
            PluginSetupDraft(
                id = "large",
                pluginId = manifest.id,
                name = "Large setup",
                revision = "d1",
                settings = values,
                expiresAt = System.currentTimeMillis() + 3_600_000,
            )
        val restoration = StateRestorationTester(compose)
        val registry = AtomicReference<androidx.compose.runtime.saveable.SaveableStateRegistry>()
        val writes = CopyOnWriteArrayList<String>()
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("/plugins") ->
                                wireJson.encodeToString(
                                    PluginCatalog(
                                        packages =
                                            listOf(
                                                PluginEntry(
                                                    id = manifest.id,
                                                    active = "1",
                                                    manifest = manifest,
                                                )
                                            )
                                    )
                                )
                            path.endsWith("/setup-drafts") ->
                                wireJson.encodeToString(PluginPage(listOf(draft)))
                            path.endsWith("/setup-drafts/large") -> wireJson.encodeToString(draft)
                            else -> null
                        }
                    }
                ) { path, _ ->
                    writes += path
                    "{}"
                }
            server.start()
            val vm =
                show(server, restoration) { model, state ->
                    val current =
                        androidx.compose.runtime.saveable.LocalSaveableStateRegistry.current
                    SideEffect { registry.set(checkNotNull(current)) }
                    PluginScreen(model, state, manifest.id) {}
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                compose.waitUntil(10000) {
                    compose
                        .onAllNodes(hasText("Continuer Large setup") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Continuer Large setup").performScrollTo().performClick()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodes(hasText("Field 6") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose
                    .onNodeWithText("Field 6")
                    .performScrollTo()
                    .performTextReplacement("edited locally")
                compose
                    .onNodeWithText("Credential")
                    .performScrollTo()
                    .performTextInput("fixture-secret")
                val bytes = compose.runOnIdle {
                    val parcel = android.os.Parcel.obtain()
                    try {
                        val saved = registry.get().performSave()
                        assertFalse(
                            "Typed credentials must stay outside saved state",
                            saved.toString().contains("fixture-secret"),
                        )
                        parcel.writeValue(saved)
                        parcel.dataSize()
                    } finally {
                        parcel.recycle()
                    }
                }
                assertTrue(
                    "A reloadable server draft must not inflate saved instance state: $bytes bytes",
                    bytes < 65_536,
                )
                println("Plugin setup saved instance state: $bytes bytes")
                restoration.emulateSavedInstanceStateRestore()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodes(hasText("Field 6") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose
                    .onNodeWithText("Field 6")
                    .performScrollTo()
                    .assertTextContains("edited locally")
                compose
                    .onNodeWithText("Field 19")
                    .performScrollTo()
                    .assertTextContains("x".repeat(10000))
                assertEquals(
                    "",
                    compose
                        .onNodeWithText("Credential")
                        .fetchSemanticsNode()
                        .config[SemanticsProperties.EditableText]
                        .text,
                )
                assertTrue(writes.isEmpty())
            } finally {
                models.clear()
            }
        }
    }

    @Test fun routeDraftSurvivesRecreation() = pluginDraftRecreation("routes")

    @Test fun grantDraftSurvivesRecreation() = pluginDraftRecreation("grants")

    @Test
    fun resourceDraftSurvivesRecreationWithoutSavingSecrets() = pluginDraftRecreation("resource")

    @Test fun setupDraftSurvivesRecreationWithoutSavingSecrets() = pluginDraftRecreation("setup")

    @Test
    fun routeDraftRetriesFailedRestorationRead() = pluginDraftRecreation("routes", failRead = true)

    @Test
    fun grantDraftRetriesFailedRestorationRead() = pluginDraftRecreation("grants", failRead = true)

    @Test
    fun resourceDraftRetriesFailedRestorationRead() =
        pluginDraftRecreation("resource", failRead = true)

    @Test
    fun setupDraftRetriesFailedRestorationRead() = pluginDraftRecreation("setup", failRead = true)

    @Test
    fun resourceCreationRestoresAReadBarrier() =
        pluginDraftRecreation("resource", pendingCreation = true)

    @Test fun laterPageGrantDraftSurvivesRecreation() = laterPageGrantRecreation(false)

    @Test fun laterPageGrantDraftRetriesAParentReadFailure() = laterPageGrantRecreation(true)

    private fun laterPageGrantRecreation(failRead: Boolean) {
        val collection = PluginResourceCollection("workspace", "Workspaces", "Workspace")
        val operations =
            listOf(
                PluginOperation("read", "workspace", PluginAccessProfile.Read, listOf("channel"))
            )
        val manifest =
            PluginManifest(
                id = "paged-restore",
                agentAccess = PluginAgentAccess(),
                resources = listOf(collection),
                operations = operations,
            )
        val intro =
            PluginResource(
                id = "intro",
                connectionId = "app",
                collection = "workspace",
                name = "Intro",
                revision = "i1",
            )
        val engineering = intro.copy(id = "engineering", name = "Engineering", revision = "e1")
        val root = "/plugins/paged-restore/connections/app/resources/workspace"
        val restoration = StateRestorationTester(compose)
        val restoring = java.util.concurrent.atomic.AtomicBoolean(false)
        val failed = java.util.concurrent.atomic.AtomicBoolean(false)
        val laterReads = AtomicInteger()
        val writes = CopyOnWriteArrayList<Pair<String, JsonObject>>()
        val rules = CopyOnWriteArrayList<PluginResourceGrant>()
        MockWebServer().use { server ->
            val normal =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith(root) ->
                                wireJson.encodeToString(PluginPage(listOf(intro), "later"))
                            path.endsWith("$root?cursor=later") -> {
                                laterReads.incrementAndGet()
                                wireJson.encodeToString(PluginPage(listOf(engineering)))
                            }
                            path.endsWith("/engineering/grants") ->
                                wireJson.encodeToString(PluginPage(rules.toList()))
                            else -> null
                        }
                    }
                ) { path, body ->
                    writes += path to wireJson.parseToJsonElement(body).jsonObject
                    val grant =
                        wireJson
                            .decodeFromString<PluginResourceGrant>(body)
                            .copy(id = "saved", resourceId = engineering.id)
                    rules += grant
                    wireJson.encodeToString(grant)
                }
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        if (
                            request.method == "GET" &&
                                request.path.orEmpty().endsWith(root) &&
                                failRead &&
                                restoring.get() &&
                                failed.compareAndSet(false, true)
                        )
                            return MockResponse()
                                .setResponseCode(503)
                                .setBody("""{"error":"Temporary fixture outage."}""")
                        return normal.dispatch(request)
                    }
                }
            server.start()
            val vm =
                show(server, restoration) { model, state ->
                    Page {
                        PluginResources(
                            model,
                            state,
                            manifest,
                            PluginResourceLocation(manifest.id, "app"),
                        )
                    }
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            fun waitEnabled(label: String) =
                compose.waitUntil(10000) {
                    compose
                        .onAllNodes(hasText(label) and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
            try {
                waitEnabled("Voir plus · Workspaces")
                compose.onNodeWithText("Voir plus · Workspaces").performScrollTo().performClick()
                compose.waitUntil(10000) {
                    compose.onAllNodesWithText("Accès des agents").fetchSemanticsNodes().size == 2
                }
                compose.onAllNodesWithText("Accès des agents")[1].performScrollTo().performClick()
                waitEnabled("Ajouter une règle")
                compose.onNodeWithText("Ajouter une règle").performScrollTo().performClick()
                compose.onNodeWithContentDescription("Helper").performScrollTo().performClick()
                compose.onNodeWithContentDescription("Lecture").performScrollTo().performClick()
                compose.onNodeWithText("Limiter channel").performScrollTo().performTextInput("C1")
                waitEnabled("Enregistrer")
                assertTrue(writes.isEmpty())
                val before = laterReads.get()
                restoring.set(true)
                restoration.emulateSavedInstanceStateRestore()
                if (failRead) {
                    compose.waitUntil(10000) {
                        failed.get() &&
                            compose
                                .onAllNodes(hasText("Actualiser les ressources") and isEnabled())
                                .fetchSemanticsNodes()
                                .isNotEmpty()
                    }
                    assertTrue(writes.isEmpty())
                    compose
                        .onNodeWithText("Actualiser les ressources")
                        .performScrollTo()
                        .performClick()
                }
                waitEnabled("Enregistrer")
                assertTrue(laterReads.get() > before)
                compose.onNodeWithText("Accès des agents · Engineering").assertExists()
                compose.onNodeWithText("Limiter channel").performScrollTo().assertTextContains("C1")
                assertTrue(writes.isEmpty())
                compose.onNodeWithText("Enregistrer").performClick()
                compose.waitUntil(10000) {
                    writes.size == 1 &&
                        compose
                            .onAllNodesWithText("Révoquer la règle")
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                }
                assertTrue(writes.single().first.endsWith("$root/engineering/grants"))
                assertEquals("helper", writes.single().second["agentId"]?.jsonPrimitive?.content)
                assertEquals(
                    listOf("C1"),
                    writes.single().second["scopes"]!!.jsonObject["channel"]!!.jsonArray.map {
                        it.jsonPrimitive.content
                    },
                )
                capture("later-page-grant-restored")
            } finally {
                models.clear()
            }
        }
    }

    private fun pluginDraftRecreation(
        kind: String,
        failRead: Boolean = false,
        pendingCreation: Boolean = false,
    ) {
        val root = "/plugins/draft-restore/connections/app/resources/workspace/engineering"
        val operations =
            listOf(
                PluginOperation(
                    name = "messages.read",
                    collection = "workspace",
                    profile = PluginAccessProfile.Read,
                    scopes = listOf("channel"),
                ),
                PluginOperation(
                    name = "messages.send",
                    collection = "workspace",
                    profile = PluginAccessProfile.Contribute,
                    scopes = listOf("channel"),
                ),
            )
        val fields =
            listOf(
                PluginSetting(key = "topic", title = "Topic"),
                PluginSetting(key = "secret", title = "Credential", type = "secret"),
            )
        val collection =
            PluginResourceCollection(
                name = "workspace",
                title = "Workspaces",
                singular = "Workspace",
                settings = fields,
            )
        val manifest =
            PluginManifest(
                id = "draft-restore",
                name = "Restore",
                settings = fields,
                resources = listOf(collection),
            )
        val draft =
            PluginSetupDraft(
                id = "draft",
                pluginId = manifest.id,
                name = "Setup",
                revision = "d1",
                expiresAt = System.currentTimeMillis() + 3_600_000,
            )
        val writes = CopyOnWriteArrayList<String>()
        val created = AtomicReference<PluginResource?>(null)
        val resourceReads = AtomicInteger()
        val restoring = java.util.concurrent.atomic.AtomicBoolean(false)
        val gated = java.util.concurrent.atomic.AtomicBoolean(false)
        val readStarted = CountDownLatch(1)
        val releaseRead = CountDownLatch(1)
        val writeStarted = CountDownLatch(1)
        val releaseWrite = CountDownLatch(1)
        val restoration = StateRestorationTester(compose)
        fun proofRead(path: String) =
            when (kind) {
                "routes" -> path.endsWith("$root/routes")
                "grants" -> path.endsWith("$root/grants")
                "resource" -> path.endsWith("/connections/app/resources/workspace")
                else -> path.endsWith("/setup-drafts/draft")
            }
        fun waitEnabled(label: String) =
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText(label) and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
        MockWebServer().use { server ->
            val normal =
                dispatcher(
                    read = { path ->
                        when {
                            path.contains("$root/route-eligibility?") ->
                                wireJson.encodeToString(
                                    PluginPage(listOf(RouteEligibility("helper", true)))
                                )
                            path.endsWith("$root/routes") || path.endsWith("$root/grants") ->
                                """{"items":[]}"""
                            path.endsWith("/resources/workspace") ->
                                wireJson.encodeToString(PluginPage(listOfNotNull(created.get())))
                            path.endsWith("/plugins/draft-restore/setup-drafts/draft") ->
                                wireJson.encodeToString(draft)
                            path.endsWith("/plugins") ->
                                wireJson.encodeToString(
                                    PluginCatalog(
                                        packages =
                                            listOf(
                                                PluginEntry(
                                                    id = manifest.id,
                                                    active = "1.0.0",
                                                    manifest = manifest,
                                                )
                                            )
                                    )
                                )
                            path.endsWith("/plugins/draft-restore/setup-drafts") ->
                                wireJson.encodeToString(PluginPage(listOf(draft)))
                            else -> null
                        }
                    }
                ) { path, body ->
                    writes += path
                    if (pendingCreation && path.endsWith("/resources/workspace")) {
                        val input = wireJson.parseToJsonElement(body).jsonObject
                        val resource =
                            PluginResource(
                                id = "created",
                                connectionId = "app",
                                collection = "workspace",
                                name = input["name"]!!.jsonPrimitive.content,
                                settings = input["settings"]!!.jsonObject,
                                secretKeys = listOf("secret"),
                                revision = "r1",
                            )
                        created.set(resource)
                        writeStarted.countDown()
                        check(releaseWrite.await(20, TimeUnit.SECONDS))
                        wireJson.encodeToString(resource)
                    } else "{}"
                }
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path.orEmpty()
                        if (request.method == "GET") {
                            if (path.endsWith("/resources/workspace"))
                                resourceReads.incrementAndGet()
                            if (
                                !pendingCreation &&
                                    restoring.get() &&
                                    proofRead(path) &&
                                    gated.compareAndSet(false, true)
                            ) {
                                readStarted.countDown()
                                check(releaseRead.await(20, TimeUnit.SECONDS))
                                if (failRead)
                                    return MockResponse()
                                        .setResponseCode(503)
                                        .setBody("""{"error":"Temporary fixture outage."}""")
                            }
                        }
                        return normal.dispatch(request)
                    }
                }
            server.start()
            val vm =
                show(server, restoration) { model, state ->
                    when (kind) {
                        "routes" -> PluginResourceRoutes(model, state, root, "Engineering", true) {}
                        "grants" ->
                            PluginResourceGrants(
                                model,
                                state,
                                "$root/grants",
                                "Engineering",
                                operations,
                                true,
                            ) {}
                        "resource" ->
                            PluginResources(
                                model,
                                state,
                                manifest,
                                PluginResourceLocation(manifest.id, "app"),
                            )
                        else -> PluginScreen(model, state, manifest.id) {}
                    }
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                val open =
                    when (kind) {
                        "routes" -> "Routes de conversation"
                        "grants" -> "Accès des agents"
                        "resource" -> "Ajouter · Workspace"
                        else -> "Continuer Setup"
                    }
                waitEnabled(open)
                if (kind == "setup") compose.onNodeWithText(open).performScrollTo()
                compose.onNodeWithText(open).performClick()
                if (kind == "routes" || kind == "grants") {
                    val add = if (kind == "routes") "Ajouter une route" else "Ajouter une règle"
                    waitEnabled(add)
                    compose.onNodeWithText(add).performScrollTo().performClick()
                }
                val field =
                    when (kind) {
                        "routes" -> "Nom de la route"
                        "grants" -> "Limiter channel"
                        "resource" -> "Nom · Workspace"
                        else -> "Nom"
                    }
                waitEnabled(field)
                compose
                    .onNodeWithText(field)
                    .performScrollTo()
                    .performTextReplacement("Retained draft")
                if (kind == "routes" || kind == "grants") {
                    compose.waitUntil(10000) {
                        compose
                            .onAllNodesWithContentDescription("Helper")
                            .fetchSemanticsNodes()
                            .any { !it.config.contains(SemanticsProperties.Disabled) }
                    }
                    compose.onNodeWithContentDescription("Helper").performScrollTo().performClick()
                    if (kind == "routes") {
                        compose
                            .onNodeWithText("Ajouter une condition")
                            .performScrollTo()
                            .performClick()
                        compose
                            .onNodeWithText("Clé de la condition 1")
                            .performScrollTo()
                            .performTextInput("channel")
                        compose
                            .onNodeWithText("Valeur de la condition 1")
                            .performScrollTo()
                            .performTextInput("C1")
                    } else
                        compose
                            .onNodeWithContentDescription("Contribution")
                            .performScrollTo()
                            .performClick()
                }
                if (kind == "resource" || kind == "setup") {
                    compose.onNodeWithText("Topic").performScrollTo().performTextInput("triage")
                    compose
                        .onNodeWithText("Credential")
                        .performScrollTo()
                        .performTextInput("fixture-secret")
                }
                val save = if (kind == "setup") "Enregistrer la configuration" else "Enregistrer"
                waitEnabled(save)
                compose.onNodeWithText(field).assertTextContains("Retained draft")
                if (pendingCreation) {
                    compose.onNodeWithText(save).performClick()
                    compose.waitUntil(10000) { writeStarted.count == 0L }
                    try {
                        restoring.set(true)
                        restoration.emulateSavedInstanceStateRestore()
                        compose.onNodeWithText(save).assertIsNotEnabled()
                        compose.onNodeWithText(field).assertTextContains("Retained draft")
                    } finally {
                        releaseWrite.countDown()
                    }
                    waitEnabled("Vérifier la sauvegarde")
                    compose.onNodeWithText(save).assertIsNotEnabled()
                    assertEquals("Recorded mutation paths: $writes", 1, writes.size)
                    val beforeRead = resourceReads.get()
                    compose
                        .onNodeWithText("Vérifier la sauvegarde")
                        .performScrollTo()
                        .performClick()
                    compose.waitUntil(10000) {
                        resourceReads.get() > beforeRead &&
                            compose
                                .onAllNodesWithText("Modifier Retained draft")
                                .fetchSemanticsNodes()
                                .isNotEmpty() &&
                            compose.onAllNodesWithText(save).fetchSemanticsNodes().isEmpty()
                    }
                    assertEquals("Recorded mutation paths: $writes", 1, writes.size)
                    capture("resource-creation-restored-with-read-barrier")
                    return
                }
                try {
                    restoring.set(true)
                    restoration.emulateSavedInstanceStateRestore()
                    compose.waitUntil(10000) { readStarted.count == 0L }
                    compose.onNodeWithText(save).assertIsNotEnabled()
                } finally {
                    releaseRead.countDown()
                }
                if (failRead) {
                    val retry =
                        when (kind) {
                            "routes" -> "Actualiser les routes"
                            "grants" -> "Actualiser les règles"
                            else -> "Réessayer le chargement"
                        }
                    compose.waitUntil(10000) {
                        compose.onAllNodesWithText(retry).fetchSemanticsNodes().isNotEmpty()
                    }
                    compose.onNodeWithText(retry).performScrollTo().assertIsEnabled().performClick()
                }
                waitEnabled(save)
                compose.onNodeWithText(field).performScrollTo().assertTextContains("Retained draft")
                if (kind == "routes")
                    compose
                        .onNodeWithText("Valeur de la condition 1")
                        .performScrollTo()
                        .assertTextContains("C1")
                if (kind == "resource" || kind == "setup") {
                    compose.onNodeWithText("Topic").performScrollTo().assertTextContains("triage")
                    assertEquals(
                        "",
                        compose
                            .onNodeWithText("Credential")
                            .fetchSemanticsNode()
                            .config[SemanticsProperties.EditableText]
                            .text,
                    )
                }
                assertTrue("Restoration recorded mutations: $writes", writes.isEmpty())
                capture("$kind-draft-restored")
            } finally {
                releaseRead.countDown()
                releaseWrite.countDown()
                models.clear()
            }
        }
    }

    private fun dispatcher(
        includeGet: Boolean = false,
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
                }

                // This synthetic Beacon has no Direct peer to authorize.
                if (
                    request.method == "POST" &&
                        path == "/api/installations/fixture/direct/authorize"
                )
                    return MockResponse().setBody("""{"available":false}""")

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
        restoration: StateRestorationTester? = null,
        content: @Composable (CairnViewModel, Workspace) -> Unit,
    ): CairnViewModel {
        val vm =
            CairnViewModel(
                ApplicationProvider.getApplicationContext<Application>(),
                PluginVault(),
                beaconOrigin = "",
            )
        val render: @Composable () -> Unit = {
            val state by vm.state.collectAsStateWithLifecycle()
            // Once loaded, the screen stays composed even if a refresh briefly empties the agents.
            var loaded by remember { mutableStateOf(false) }
            if (state.agents.isNotEmpty()) loaded = true
            LaunchedEffect(Unit) {
                vm.state.first { it.ready }
                if (vm.state.value.installation == null) vm.connect(server.url("/").toString())
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
        if (restoration == null) compose.setContent(render) else restoration.setContent(render)
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
