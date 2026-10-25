package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
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
    fun compactCatalogueCompletesItsPagesWithoutLoadingEveryInstanceSetting() {
        val reads = CopyOnWriteArrayList<String>()
        fun card(id: String, name: String) =
            PluginCardEntry(
                id,
                true,
                "active",
                "1",
                versionCount = 1,
                presentation =
                    PluginPresentation(
                        name,
                        "Compact package",
                        "1",
                        capabilities = emptyList(),
                        needsWorker = false,
                        delegates = false,
                        agentAccess = true,
                        contributions =
                            PluginContributionCounts(1, 1, false, true, false, false, 0),
                    ),
                summary = PluginInstanceSummary(3, mapOf("ready" to 1, "paused" to 2), 4, 2),
            )
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        reads += path
                        when {
                            path.endsWith("/plugins/packages") ->
                                wireJson.encodeToString(
                                    PluginCards(
                                        packageReads = true,
                                        instanceReads = true,
                                        items = listOf(card("onepassword", "1Password")),
                                        nextCursor = "onepassword",
                                    )
                                )
                            path.endsWith("/plugins/packages?cursor=onepassword") ->
                                wireJson.encodeToString(
                                    PluginCards(
                                        packageReads = true,
                                        instanceReads = true,
                                        items = listOf(card("notes", "Notes")),
                                    )
                                )
                            else -> null
                        }
                    }
                ) { _, _ ->
                    null
                }
            server.start()
            var opened = ""
            val vm = show(server) { model, state -> PluginsScreen(model, state) { opened = it } }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                compose.waitUntil(10000) {
                    compose.onAllNodesWithTag("plugin-notes").fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNodeWithTag("plugin-onepassword").assertExists()
                compose.onAllNodesWithText("4 demandes").assertCountEquals(2)
                assertFalse(
                    "Cards must not load the complete catalogue",
                    reads.any { it.endsWith("/api/plugins") },
                )
                capture("plugins-compact-catalogue")
                compose.onNodeWithTag("plugin-onepassword").performClick()
                assertEquals("onepassword", opened)
            } finally {
                models.clear()
            }
        }
    }

    @Test
    fun reviewApprovesOnlyTheExactDisplayedVersionAndIgnoresLateReads() {
        val entry =
            notes.copy(
                id = "review",
                builtin = false,
                status = "update",
                active = "1",
                versions = emptyList(),
            )
        val lateStarted = CountDownLatch(1)
        val releaseLate = CountDownLatch(1)
        val approvalStarted = CountDownLatch(1)
        val releaseApproval = CountDownLatch(1)
        val writes = CopyOnWriteArrayList<JsonObject>()
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        val root = "/plugins/packages/review"
                        when {
                            path.endsWith(root) -> wireJson.encodeToString(entry)
                            path.endsWith("$root/versions") ->
                                wireJson.encodeToString(
                                    PluginPage(
                                        listOf(
                                            PluginVersion(
                                                version = "1",
                                                digest = "old",
                                                approved = true,
                                            )
                                        ),
                                        "1",
                                    )
                                )
                            path.endsWith("$root/versions?cursor=1") ->
                                wireJson.encodeToString(
                                    PluginPage(
                                        listOf(
                                            PluginVersion(
                                                version = "2",
                                                digest = "metadata-two",
                                                approved = false,
                                            ),
                                            PluginVersion(
                                                version = "3",
                                                digest = "metadata-three",
                                                approved = false,
                                            ),
                                        )
                                    )
                                )
                            path.endsWith("$root/versions/3") -> {
                                lateStarted.countDown()
                                check(releaseLate.await(30, TimeUnit.SECONDS))
                                wireJson.encodeToString(
                                    PluginVersion(
                                        version = "3",
                                        digest = "late-three",
                                        approved = false,
                                        manifest =
                                            notes.manifest!!.copy(
                                                name = "Late version three",
                                                version = "3",
                                            ),
                                    )
                                )
                            }
                            path.endsWith("$root/versions/2") ->
                                wireJson.encodeToString(
                                    PluginVersion(
                                        version = "2",
                                        digest = "reviewed-two",
                                        approved = false,
                                        manifest =
                                            notes.manifest!!.copy(
                                                name = "Inspected version two",
                                                version = "2",
                                                capabilities = listOf("http"),
                                            ),
                                        files = listOf(PluginFile("lib/review.luau", 100)),
                                    )
                                )
                            path.endsWith("$root/versions/1") ->
                                wireJson.encodeToString(
                                    PluginVersion(
                                        version = "1",
                                        digest = "old",
                                        approved = true,
                                        manifest =
                                            notes.manifest!!.copy(
                                                version = "1",
                                                capabilities = emptyList(),
                                            ),
                                    )
                                )
                            else -> null
                        }
                    }
                ) { path, body ->
                    if (path.endsWith("/plugins/packages/review/approve")) {
                        writes += wireJson.parseToJsonElement(body).jsonObject
                        approvalStarted.countDown()
                        check(releaseApproval.await(30, TimeUnit.SECONDS))
                        "{}"
                    } else null
                }
            server.start()
            var closed by mutableStateOf(false)
            val vm =
                show(server) { model, state ->
                    if (!closed)
                        PluginReview(
                            model,
                            state,
                            entry.configuration(),
                            emptyList(),
                            { closed = true },
                        ) {}
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                compose.waitUntil(10000) { lateStarted.count == 0L }
                compose.onAllNodesWithText("Approuver et activer").assertCountEquals(0)
                compose.onNodeWithText("2").performScrollTo().performClick()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Inspected version two 2")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                releaseLate.countDown()
                compose.waitForIdle()
                compose.onNodeWithText("Inspected version two 2").assertExists()
                compose.onAllNodesWithText("Late version three 3").assertCountEquals(0)
                compose.onNodeWithText("Nouveau").assertExists()
                capture("plugins-exact-version-review")
                compose
                    .onNodeWithText("Approuver et activer")
                    .assertIsEnabled()
                    .assertIsDisplayed()
                    .performClick()
                compose.waitUntil(10000) { approvalStarted.count == 0L }
                releaseApproval.countDown()
                compose.waitUntil(10000) {
                    // Observe the UI completion as well as the HTTP write. This
                    // drives the Robolectric main loop while the response resumes.
                    val dismissed =
                        compose.onAllNodesWithTag("plugin-review").fetchSemanticsNodes().isEmpty()
                    writes.size == 1 && dismissed
                }
                assertEquals("2", writes.single()["version"]!!.jsonPrimitive.content)
                assertEquals("reviewed-two", writes.single()["digest"]!!.jsonPrimitive.content)
            } finally {
                releaseLate.countDown()
                releaseApproval.countDown()
                models.clear()
            }
        }
    }

    @Test
    fun missingSelectedPackageShowsNotInstalledFromCompactSupport() {
        val paths = CopyOnWriteArrayList<String>()
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        paths += path
                        if (path.endsWith("/plugins/packages"))
                            wireJson.encodeToString(
                                PluginCards(
                                    packageReads = true,
                                    instanceReads = true,
                                    accessReads = true,
                                    items = emptyList(),
                                )
                            )
                        else null
                    }
                ) { _, _ ->
                    null
                }
            server.start()
            val vm = show(server) { model, state -> PluginScreen(model, state, "removed") {} }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Plugin introuvable")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                assertFalse(paths.any { it.endsWith("/api/plugins") })
                assertTrue(paths.any { it.endsWith("/plugins/packages/removed") })
                capture("plugin-selected-package-missing")
            } finally {
                models.clear()
            }
        }
    }

    @Test
    fun selectedPackagePublicChoicesWarnAboutOtherPackagesResourceAccess() {
        val paths = CopyOnWriteArrayList<String>()
        val instance =
            PluginConnection(
                id = "team",
                pluginId = "notes",
                name = "Team notes",
                settings = buildJsonObject { put("team", "core") },
                agentId = "helper",
                accessMode = "everyone",
            )
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        paths += path
                        when {
                            path.endsWith("/plugins/packages/notes") ->
                                wireJson.encodeToString(
                                    notes
                                        .configuration()
                                        .copy(
                                            instanceReads = true,
                                            accessReads = true,
                                            capabilities = emptyList(),
                                        )
                                )
                            path.endsWith("/plugins/packages/notes/connections") ->
                                wireJson.encodeToString(PluginPage(listOf(instance)))
                            path.endsWith("/plugins/instance-access") ->
                                """{"items":[],"nextCursor":"next"}"""
                            path.endsWith("/plugins/instance-access?cursor=next") ->
                                wireJson.encodeToString(
                                    PluginPage(
                                        listOf(
                                            PluginInstanceAccess(
                                                "other",
                                                "missing",
                                                "missing",
                                                "Other workspace",
                                                false,
                                                emptyList(),
                                                listOf("helper"),
                                            )
                                        )
                                    )
                                )
                            else -> null
                        }
                    }
                ) { _, _ ->
                    null
                }
            server.start()
            val vm =
                show(server) { model, state ->
                    PluginScreen(
                        model,
                        state.copy(
                            agents =
                                state.agents.map {
                                    if (it.id == "helper")
                                        it.copy(
                                            access =
                                                AccessPolicy(
                                                    projects = emptyList(),
                                                    mcps = emptyList(),
                                                    sandbox = "read-only",
                                                    delegation = true,
                                                    backend = "native",
                                                )
                                        )
                                    else it
                                }
                        ),
                        "notes",
                    ) {}
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                compose.waitUntil(10000) {
                    compose.onAllNodesWithText("Modifier").fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNodeWithText("Modifier").performScrollTo().performClick()
                compose
                    .onNode(
                        hasContentDescription("Qui peut lui parler", substring = true) and
                            hasClickAction()
                    )
                    .performClick()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("utiliser ses plugins", substring = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose
                    .onNodeWithText("utiliser ses plugins", substring = true)
                    .performScrollTo()
                    .assertIsDisplayed()
                compose.onNodeWithText("Enregistrer").assertIsEnabled()
                compose.onNodeWithText("déléguer du travail", substring = true).assertExists()
                compose
                    .onNodeWithText(
                        "Les messages externes ne peuvent pas démarrer",
                        substring = true,
                    )
                    .performScrollTo()
                    .assertIsDisplayed()
                capture("plugin-selected-package-public-resource-access")
                compose.onNodeWithText("Tout le monde").performScrollTo().performClick()
                compose.onNodeWithText("Valider les personnes à leur demande").performClick()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("utiliser ses plugins", substring = true)
                        .fetchSemanticsNodes()
                        .isEmpty()
                }
                compose.onNodeWithText("déléguer du travail", substring = true).assertExists()
                compose.onNodeWithText("Enregistrer").assertIsEnabled()
                assertFalse(paths.any { it.endsWith("/api/plugins") })
                assertTrue(paths.any { it.endsWith("/instance-access?cursor=next") })
            } finally {
                models.clear()
            }
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
    fun ordinaryFieldsClearStaleErrorsAndPresetsRefreshDependentChoices() {
        val entry =
            notes.copy(
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
                                                            mapOf("more" to true, "extra" to true),
                                                    ),
                                                ),
                                        )
                                    )
                            ),
                    )
            )
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
                            wireJson.encodeToString(
                                PluginCatalog(setupDrafts = true, packages = listOf(entry))
                            )
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
            assertEquals("helper", payload["agentId"]?.jsonPrimitive?.content)
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
                        )
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
            assertEquals(JsonArray(listOf(JsonPrimitive("helper"))), payload["agentIds"])
            assertEquals(JsonObject(emptyMap()), payload["settings"])
        }
    }

    @Test fun anOwnerResumesSetupAndActivatesItsPreparedResources() = ownerSetup(false)

    @Test fun setupFormsAndActivationFollowManagerOperations() = ownerSetup(true)

    private fun ownerSetup(tracked: Boolean) {
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
        val operations = java.util.concurrent.ConcurrentHashMap<String, String>()
        val operationReads = AtomicInteger()
        fun operation(id: String, result: String): String {
            if (!tracked) return result
            operations[id] = result
            return buildJsonObject { put("operation", id) }.toString()
        }
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        when {
                            path.contains("/plugins/packages/operations/") -> {
                                operationReads.incrementAndGet()
                                val id = path.substringAfterLast('/')
                                val result =
                                    operations[id] ?: error("Unknown fixture operation $id")
                                if (id == "activate") {
                                    active.set(wireJson.decodeFromString<PluginConnection>(result))
                                    draft.set(null)
                                }
                                buildJsonObject {
                                    put("state", "completed")
                                    put("result", wireJson.parseToJsonElement(result))
                                }
                                    .toString()
                            }
                            path.endsWith("/plugins") ->
                                wireJson.encodeToString(
                                    PluginCatalog(
                                        setupDrafts = true,
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
                            operation(
                                "form-${writes.size}",
                                """{"options":{},"values":{"setupIdentity":"setup1"},"errors":{},"view":[{"type":"alert","tone":"info","title":"Prepare the service","text":"Save settings before adding workspaces."}]}""",
                            )
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
                            val connection =
                                PluginConnection(
                                    id = "setup1",
                                    pluginId = "workspace-service",
                                    name = "Production service",
                                    settings = draft.get()!!["settings"]!!.jsonObject,
                                    status = PluginStatus("ready"),
                                )
                            if (!tracked) {
                                active.set(connection)
                                draft.set(null)
                            }
                            operation("activate", wireJson.encodeToString(connection))
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
            if (tracked) assertTrue(operationReads.get() > 0)
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
            // Follow the actual screen so Robolectric processes the response and
            // subsequent manager polling on the UI thread before inspecting the fixture.
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Configurer Workspace service")
                    .fetchSemanticsNodes()
                    .isEmpty()
            }
            assertNotNull(active.get())
            assertNull(draft.get())
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
                                wireJson.encodeToString(
                                    PluginCatalog(setupDrafts = true, packages = listOf(entry))
                                )
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
                                wireJson.encodeToString(
                                    PluginCatalog(setupDrafts = true, packages = listOf(entry))
                                )
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
                                wireJson.encodeToString(
                                    PluginCatalog(setupDrafts = true, packages = listOf(entry))
                                )
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

    @Test fun preparedRootOAuthUsesItsSavedRevisionAndCanBeCancelled() = rootOAuthConsent(false)

    @Test fun preparedRootOAuthSavesTheCompletedFlowWithoutAClientToken() = rootOAuthConsent(true)

    @Test
    fun closingRootOAuthRetiresAConsentWhoseStartResponseArrivesLate() =
        rootOAuthConsent(false, true)

    private fun rootOAuthConsent(complete: Boolean, closeDuringStart: Boolean = false) {
        val starts = CopyOnWriteArrayList<JsonObject>()
        val patches = CopyOnWriteArrayList<JsonObject>()
        val completed = java.util.concurrent.atomic.AtomicBoolean(false)
        val cancelled = java.util.concurrent.atomic.AtomicBoolean(false)
        val releaseStart = CountDownLatch(1)
        val visible = mutableStateOf(true)
        val draft =
            PluginSetupDraft(
                id = "draft-root",
                pluginId = "root-consent-ui",
                name = "Root service",
                settings = buildJsonObject { put("clientId", "draft-client-ui") },
                revision = "saved-revision",
                expiresAt = System.currentTimeMillis() + 600000,
            )
        val root = "/plugins/${draft.pluginId}/setup-drafts/${draft.id}"
        val flowPath = "/plugins/${draft.pluginId}/oauth/oauth-root"
        val flow =
            PluginDeviceFlow(
                id = "oauth-root",
                pluginId = draft.pluginId,
                verificationUri = "https://provider.example.test/authorize?state=synthetic",
                expiresAt = System.currentTimeMillis() + 600000,
            )
        val manifest =
            PluginManifest(
                id = draft.pluginId,
                name = "Root service",
                settings =
                    listOf(PluginSetting(key = "clientId", title = "Client ID", required = true)),
                credential =
                    PluginCredential(
                        label = "Service account",
                        oauth =
                            buildJsonObject {
                                put("clientIdSetting", "clientId")
                                put("authorizationUrl", "https://provider.example.test/authorize")
                                put("tokenUrl", "https://provider.example.test/token")
                            },
                    ),
                resources =
                    listOf(
                        PluginResourceCollection(
                            name = "workspace",
                            title = "Workspaces",
                            singular = "Workspace",
                        )
                    ),
            )
        MockWebServer().use { server ->
            val fallback = dispatcher { _, _ -> null }
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path.orEmpty()
                        if (path.endsWith(root) && request.method == "GET")
                            return MockResponse().setBody(wireJson.encodeToString(draft))
                        if (path.endsWith(root) && request.method == "PATCH") {
                            val body =
                                wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                            assertEquals("saved-revision", body["revision"]?.jsonPrimitive?.content)
                            assertEquals(flow.id, body["credentialFlow"]?.jsonPrimitive?.content)
                            assertFalse(body.containsKey("token"))
                            patches += body
                            return MockResponse()
                                .setBody(
                                    wireJson.encodeToString(
                                        draft.copy(revision = "consent-saved", credential = true)
                                    )
                                )
                        }
                        if (path.endsWith("$root/oauth") && request.method == "POST") {
                            assertEquals("fixture-csrf", request.getHeader("X-CSRF-Token"))
                            starts +=
                                wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                            if (closeDuringStart)
                                assertTrue(releaseStart.await(10, TimeUnit.SECONDS))
                            return MockResponse().setBody(wireJson.encodeToString(flow))
                        }
                        if (path.endsWith(flowPath)) {
                            if (request.method == "DELETE") {
                                assertEquals("fixture-csrf", request.getHeader("X-CSRF-Token"))
                                cancelled.set(true)
                                return MockResponse().setBody("{}")
                            }
                            return MockResponse()
                                .setBody(
                                    wireJson.encodeToString(
                                        if (completed.get())
                                            flow.copy(state = "completed", verificationUri = "")
                                        else flow
                                    )
                                )
                        }
                        if (path.endsWith("$root/resources/workspace"))
                            return MockResponse().setBody("""{"items":[]}""")
                        return fallback.dispatch(request)
                    }
                }
            server.start()
            show(server) { vm, state ->
                if (visible.value)
                    PluginSetup(
                        vm,
                        state,
                        manifest,
                        draft,
                        close = { visible.value = false },
                        changed = {},
                        activated = {},
                    )
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Se connecter avec Root service") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNode(hasText("Service account") and hasSetTextAction()).assertDoesNotExist()
            compose
                .onNodeWithText("Se connecter avec Root service")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) { starts.isNotEmpty() }
            assertEquals(buildJsonObject { put("revision", draft.revision) }, starts.single())
            if (closeDuringStart) {
                compose.onNodeWithContentDescription("Fermer").performClick()
                compose.onNodeWithText("Configurer Root service").assertDoesNotExist()
                releaseStart.countDown()
                compose.waitUntil(10000) {
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    cancelled.get()
                }
                return@use
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Ouvrir la page de connexion")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Activer l’instance").assertIsNotEnabled()
            capture("plugin-root-oauth-pending")
            if (complete) {
                completed.set(true)
                // Polling uses Compose's test clock. Drive its next two-second
                // interval before waiting for the real HTTP response and UI.
                compose.mainClock.advanceTimeBy(2000)
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText(
                            "Connexion terminée. Enregistrez la configuration pour garder ce compte."
                        )
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Activer l’instance").assertIsNotEnabled()
                compose
                    .onNodeWithText(
                        "Connexion terminée. Enregistrez la configuration pour garder ce compte."
                    )
                    .performScrollTo()
                capture("plugin-root-oauth-complete")
                compose
                    .onNodeWithText("Enregistrer la configuration")
                    .performScrollTo()
                    .performClick()
                compose.waitUntil(10000) { patches.isNotEmpty() }
                compose.waitUntil(10000) {
                    compose
                        .onAllNodes(hasText("Activer l’instance") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                assertFalse(cancelled.get())
            } else {
                compose.onNodeWithText("Annuler la connexion").performScrollTo().performClick()
                compose.waitUntil(10000) { cancelled.get() }
                compose.onNodeWithText("Ouvrir la page de connexion").assertDoesNotExist()
                compose.onNodeWithText("Se connecter avec Root service").assertIsEnabled()
            }
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

    @Test
    fun resourceOAuthStatusesLoadOnlyForOpenedProvidersAcrossRevisions() {
        val revision = AtomicReference("resource0")
        val statusReads = AtomicInteger()
        val manifest =
            PluginManifest(
                id = "owner-limits",
                name = "Owner limits",
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
                    (0 until 8).map { index ->
                        PluginOAuthProvider(
                            name = "provider$index",
                            title = "Provider $index",
                            collection = "workspace",
                            scopes = listOf("messages:read"),
                        )
                    },
            )
        val connection =
            PluginConnection(
                id = "app",
                pluginId = manifest.id,
                name = "Application",
                status = PluginStatus("ready"),
            )
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
                                            ),
                                        connections = listOf(connection),
                                    )
                                )
                            path.substringBefore('?').endsWith("/resources/workspace") ->
                                wireJson.encodeToString(
                                    PluginPage(
                                        (0 until 2).map { index ->
                                            PluginResource(
                                                id = "resource$index",
                                                connectionId = connection.id,
                                                collection = "workspace",
                                                name = "Workspace $index",
                                                revision = revision.get(),
                                            )
                                        }
                                    )
                                )
                            path.contains("/oauth/provider") -> {
                                statusReads.incrementAndGet()
                                """{"status":"disconnected"}"""
                            }
                            else -> null
                        }
                    }
                ) { _, _ ->
                    null
                }
            server.start()
            show(server) { model, state -> PluginScreen(model, state, manifest.id) {} }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Provider 7", substring = false)
                    .fetchSemanticsNodes()
                    .size == 2
            }
            compose.waitForIdle()
            assertEquals(
                "Listing resources must not fetch every provider status",
                0,
                statusReads.get(),
            )
            compose
                .onAllNodesWithText("Gérer l’autorisation", substring = false)[0]
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                // Read semantics first so Robolectric advances the pending UI work.
                val visibleScopes =
                    compose
                        .onAllNodesWithText("messages:read", substring = false)
                        .fetchSemanticsNodes()
                        .size
                visibleScopes == 1 && statusReads.get() == 1
            }
            compose.onNodeWithText("messages:read", substring = false).assertExists()
            revision.set("resource1")
            compose.onNodeWithText("Actualiser les ressources").performScrollTo().performClick()
            compose.waitUntil(10000) {
                // Read semantics first so Robolectric advances the pending UI work.
                val visibleScopes =
                    compose
                        .onAllNodesWithText("messages:read", substring = false)
                        .fetchSemanticsNodes()
                        .size
                visibleScopes == 1 && statusReads.get() == 2
            }
            compose.waitForIdle()
            assertEquals("Only the provider the owner opened is refreshed", 2, statusReads.get())
            compose.onNodeWithText("messages:read", substring = false).assertExists()
            capture("resource-authorization-on-demand")
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
                                        setupDrafts = true,
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
            openSharedResourceAuthorization()
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
            openSharedResourceAuthorization()
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
            // The active namespace starts with Intro, then Engineering.
            openSharedResourceAuthorization(1)
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
                                        setupDrafts = true,
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
                openSharedResourceAuthorization()
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
                                        setupDrafts = true,
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
            openSharedResourceAuthorization()
            openSharedResourceAuthorization()
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
                                        setupDrafts = true,
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
    fun resourceRouteSummariesKeepUnknownAndFailedReadsDistinctFromZero() {
        val root = "/plugins/summary/connections/app/resources/workspace/engineering"
        val started = CountDownLatch(1)
        val release = CountDownLatch(1)
        val reads = AtomicInteger()
        val mode = AtomicInteger()
        val period = 1_700_002_800_000L
        MockWebServer().use { server ->
            val normal = dispatcher { _, _ -> null }
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        if (
                            request.method != "GET" ||
                                !request.path.orEmpty().endsWith("$root/routes")
                        )
                            return normal.dispatch(request)
                        if (reads.getAndIncrement() == 0) {
                            started.countDown()
                            check(release.await(20, TimeUnit.SECONDS))
                        }
                        if (mode.get() == 1)
                            return MockResponse()
                                .setResponseCode(503)
                                .setBody("""{"error":"Summary fixture unavailable."}""")
                        val changed = mode.get() == 2
                        return MockResponse()
                            .setBody(
                                wireJson.encodeToString(
                                    PluginPage(
                                        listOf(
                                            PluginRoute(
                                                id = "support",
                                                name = "Support",
                                                agentId = "helper",
                                                summary =
                                                    PluginRouteSummary(
                                                        period,
                                                        if (changed) 8 else 7,
                                                        if (changed) 1 else 2,
                                                    ),
                                            ),
                                            PluginRoute(
                                                id = "paused",
                                                name = "Paused",
                                                enabled = false,
                                                agentId = "helper",
                                                maxMessagesPerHour = 10,
                                                summary = PluginRouteSummary(period, 3, 0),
                                            ),
                                            PluginRoute(
                                                id = "older",
                                                name = "Earlier host",
                                                agentId = "helper",
                                            ),
                                        )
                                    )
                                )
                            )
                    }
                }
            server.start()
            val vm =
                show(server) { model, state ->
                    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                        PluginResourceRoutes(model, state, root, "Engineering", true) {}
                    }
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Routes de conversation")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Routes de conversation").performClick()
                compose.waitUntil(10000) { started.count == 0L }
                compose.onNodeWithText("0 demandes d’accès en attente").assertDoesNotExist()
                compose.onNodeWithText("7 / 20 messages").assertDoesNotExist()
                release.countDown()
                compose.waitUntil(10000) {
                    compose.onAllNodesWithText("7 / 20 messages").fetchSemanticsNodes().isNotEmpty()
                }
                compose
                    .onNodeWithText("2 demandes d’accès en attente")
                    .performScrollTo()
                    .assertExists()
                compose
                    .onAllNodesWithText("Heure commençant le ${date(period)}")
                    .assertCountEquals(2)
                compose.onNodeWithText("Paused · En pause").performScrollTo().assertExists()
                compose.onNodeWithText("3 / 10 messages").performScrollTo().assertExists()
                compose
                    .onNodeWithText("0 demandes d’accès en attente")
                    .performScrollTo()
                    .assertExists()
                compose
                    .onNodeWithText("Usage et nombre de demandes indisponibles.")
                    .performScrollTo()
                    .assertExists()
                capture("route-summary-known-and-unknown")
                mode.set(1)
                compose.onNodeWithText("Actualiser les routes").performScrollTo().performClick()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Summary fixture unavailable.", substring = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("7 / 20 messages").assertDoesNotExist()
                compose.onNodeWithText("0 demandes d’accès en attente").assertDoesNotExist()
                mode.set(2)
                compose.onNodeWithText("Actualiser les routes").performScrollTo().performClick()
                compose.waitUntil(10000) {
                    compose.onAllNodesWithText("8 / 20 messages").fetchSemanticsNodes().isNotEmpty()
                }
                compose
                    .onNodeWithText("1 demande d’accès en attente")
                    .performScrollTo()
                    .assertExists()
                capture("route-summary-refreshed")
            } finally {
                release.countDown()
                models.clear()
            }
        }
    }

    @Test
    fun resourceRouteWarningsFollowTheCurrentParticipationMode() {
        val root = "/plugins/warnings/connections/app/resources/workspace/engineering"
        val previews = AtomicInteger()
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("$root/routes") ->
                                wireJson.encodeToString(
                                    PluginPage(
                                        listOf(
                                            PluginRoute(
                                                id = "support",
                                                name = "Support",
                                                agentId = "helper",
                                                accessMode = "everyone",
                                                match = mapOf("channel" to "C1"),
                                            )
                                        )
                                    )
                                )
                            path.contains("$root/route-eligibility?") -> {
                                previews.incrementAndGet()
                                wireJson.encodeToString(
                                    PluginPage(
                                        listOf(
                                            RouteEligibility(
                                                agentId = "helper",
                                                eligible = true,
                                                pluginAccess = true,
                                            )
                                        )
                                    )
                                )
                            }
                            else -> null
                        }
                    }
                ) { _, _ ->
                    null
                }
            server.start()
            val helper =
                Agent(
                    id = "helper",
                    name = "Resource helper",
                    access =
                        AccessPolicy(
                            projects = emptyList(),
                            mcps = emptyList(),
                            sandbox = "read-only",
                            delegation = true,
                            backend = "native",
                        ),
                )
            val vm =
                show(server) { model, state ->
                    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                        PluginResourceRoutes(
                            model,
                            state.copy(agents = listOf(helper)),
                            root,
                            "Engineering",
                            true,
                        ) {}
                    }
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Routes de conversation")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Routes de conversation").performClick()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Modifier la route Support")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Modifier la route Support").performScrollTo().performClick()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("utiliser ses plugins", substring = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose
                    .onNodeWithText("utiliser ses plugins", substring = true)
                    .performScrollTo()
                    .assertIsDisplayed()
                compose.onNodeWithText("déléguer du travail", substring = true).assertExists()
                compose
                    .onNodeWithText(
                        "Les messages externes ne peuvent pas démarrer",
                        substring = true,
                    )
                    .performScrollTo()
                    .assertIsDisplayed()
                capture("resource-route-plugin-warning")
                compose.onNodeWithText("Tout le monde").performScrollTo().performClick()
                compose.onNodeWithText("Approuver sur demande").performClick()
                compose.waitUntil(10000) {
                    previews.get() >= 2 &&
                        compose
                            .onAllNodesWithText("utiliser ses plugins", substring = true)
                            .fetchSemanticsNodes()
                            .isEmpty()
                }
                compose.waitUntil(10000) {
                    compose
                        .onAllNodes(hasText("Enregistrer") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("déléguer du travail", substring = true).assertExists()
                compose.onNodeWithText("Enregistrer").assertIsEnabled()
            } finally {
                models.clear()
            }
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
                        setupDrafts = true,
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
    fun closedLargeRouteEditorsKeepActivityStateSmall() {
        val root = "/plugins/large-routes/connections/app/resources/workspace"
        val users = (0 until 100).map { it.toString().padStart(64, '0') }
        val routes =
            (0 until 16).map { index ->
                PluginRoute(
                    id = "route$index",
                    resourceId = "resource$index",
                    name = "Route $index",
                    match = mapOf("channel" to "C$index"),
                    agentId = "helper",
                    userIds = users,
                    people = users.map { PluginLabel(it, "😀".repeat(100)) },
                    revision = "revision$index",
                )
            }
        val writes = CopyOnWriteArrayList<String>()
        val restoration = StateRestorationTester(compose)
        val registry = AtomicReference<androidx.compose.runtime.saveable.SaveableStateRegistry>()
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { requested ->
                        val route = routes.find {
                            requested.endsWith("$root/${it.resourceId}/routes")
                        }
                        when {
                            route != null -> wireJson.encodeToString(PluginPage(listOf(route)))
                            requested.contains(root) && requested.contains("/route-eligibility?") ->
                                wireJson.encodeToString(
                                    PluginPage(listOf(RouteEligibility("helper", true)))
                                )
                            else -> null
                        }
                    }
                ) { requested, _ ->
                    if (requested.contains(root)) writes += requested
                    null
                }
            server.start()
            val vm =
                show(server, restoration) { model, state ->
                    val current =
                        androidx.compose.runtime.saveable.LocalSaveableStateRegistry.current
                    SideEffect { registry.set(checkNotNull(current)) }
                    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                        routes.forEach { route ->
                            key(route.resourceId) {
                                PluginResourceRoutes(
                                    model,
                                    state,
                                    "$root/${route.resourceId}",
                                    route.resourceId,
                                    true,
                                ) {}
                            }
                        }
                    }
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Routes de conversation")
                        .fetchSemanticsNodes()
                        .size == routes.size
                }
                routes.forEachIndexed { index, route ->
                    compose
                        .onAllNodesWithText("Routes de conversation")[index]
                        .performScrollTo()
                        .performClick()
                    compose.waitUntil(10000) {
                        compose
                            .onAllNodes(hasText("Modifier la route ${route.name}") and isEnabled())
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    compose
                        .onNodeWithText("Modifier la route ${route.name}")
                        .performScrollTo()
                        .performClick()
                    compose.onNodeWithText("Nom de la route").assertTextContains(route.name)
                    if (index % 2 == 0)
                        compose
                            .onNodeWithText("Annuler la modification")
                            .performScrollTo()
                            .performClick()
                    compose.onNodeWithContentDescription("Fermer").performClick()
                    compose.waitForIdle()
                }
                val bytes = compose.runOnIdle {
                    val parcel = android.os.Parcel.obtain()
                    try {
                        parcel.writeValue(registry.get().performSave())
                        parcel.dataSize()
                    } finally {
                        parcel.recycle()
                    }
                }
                println("Activity state after closing 16 large route editors: $bytes bytes")
                assertTrue(
                    "Closed route editors must not inflate activity state: $bytes",
                    bytes < 65536,
                )
                restoration.emulateSavedInstanceStateRestore()
                compose.onAllNodesWithText("Routes de conversation").assertCountEquals(routes.size)
                assertTrue("Restoration must not submit a route", writes.isEmpty())
            } finally {
                models.clear()
            }
        }
    }

    @Test
    fun largeRouteInputsRestoreWithoutSavingThePeopleDirectory() = routeInputRestoration("saved")

    @Test fun missingLocalRouteInputsRequireAnExplicitRestart() = routeInputRestoration("missing")

    @Test fun replacedRouteRevisionCannotRestoreAnEditableDraft() = routeInputRestoration("changed")

    @Test fun deletedRouteCannotRestoreAsANewRoute() = routeInputRestoration("deleted")

    @Test
    fun closedPermissionWidgetsDoNotEvictActiveRouteInputs() =
        routeInputRestoration("closed-grants")

    private fun routeInputRestoration(recovery: String) {
        if (recovery == "closed-grants")
            PluginSettingsDraftStore.clear(ApplicationProvider.getApplicationContext())
        val root = "/plugins/route-inputs/connections/app/resources/workspace/engineering"
        val users = (0 until 100).map { it.toString().padStart(64, '0') }
        val initial =
            PluginRoute(
                id = "route",
                resourceId = "engineering",
                name = "Existing route",
                match = mapOf("channel" to "original"),
                agentId = "helper",
                userIds = users,
                people = users.map { PluginLabel(it, "😀".repeat(100)) },
                revision = "r1",
            )
        val current = AtomicReference<PluginRoute?>(initial)
        val writes = CopyOnWriteArrayList<Pair<String, JsonObject>>()
        val restoring = java.util.concurrent.atomic.AtomicBoolean(false)
        val restoredRead = CountDownLatch(1)
        val releaseRestoredRead = CountDownLatch(1)
        val restoration = StateRestorationTester(compose)
        val registry = AtomicReference<androidx.compose.runtime.saveable.SaveableStateRegistry>()
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("$root/routes") -> {
                                if (recovery == "missing" && restoring.get()) {
                                    restoredRead.countDown()
                                    check(releaseRestoredRead.await(30, TimeUnit.SECONDS))
                                }
                                wireJson.encodeToString(PluginPage(listOfNotNull(current.get())))
                            }
                            path.contains("$root/route-eligibility?") ->
                                wireJson.encodeToString(
                                    PluginPage(listOf(RouteEligibility("helper", true)))
                                )
                            else -> null
                        }
                    }
                ) { path, body ->
                    if (path.contains("$root/routes")) {
                        writes += path to wireJson.parseToJsonElement(body).jsonObject
                        "{}"
                    } else null
                }
            server.start()
            val vm =
                show(server, restoration) { model, state ->
                    val owner = androidx.compose.runtime.saveable.LocalSaveableStateRegistry.current
                    SideEffect { registry.set(checkNotNull(owner)) }
                    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                        PluginResourceRoutes(model, state, root, "Engineering", true) {}
                        if (recovery == "closed-grants")
                            repeat(96) { index ->
                                key(index) {
                                    PluginResourceGrants(
                                        model,
                                        state,
                                        "$root/closed$index/grants",
                                        "Closed $index",
                                        listOf(
                                            PluginOperation(
                                                "read",
                                                "workspace",
                                                PluginAccessProfile.Read,
                                                listOf("channel"),
                                            )
                                        ),
                                        true,
                                    ) {}
                                }
                            }
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
                waitEnabled("Routes de conversation")
                compose.onNodeWithText("Routes de conversation").performClick()
                waitEnabled("Modifier la route Existing route")
                compose
                    .onNodeWithText("Modifier la route Existing route")
                    .performScrollTo()
                    .performClick()
                compose.onNodeWithText("Nom de la route").performTextReplacement("Draft route")
                compose
                    .onNodeWithText("Valeur de la condition 1")
                    .performScrollTo()
                    .performTextReplacement("support")
                compose
                    .onNodeWithText("Identifiants des personnes autorisées")
                    .performScrollTo()
                    .performTextReplacement(users.dropLast(1).joinToString("\n"))
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
                    "Active route inputs must not inflate activity state: $bytes",
                    bytes < if (recovery == "closed-grants") 262144 else 65536,
                )
                println("Active route input activity state ($recovery): $bytes bytes")
                if (recovery == "closed-grants") {
                    val snapshots =
                        ApplicationProvider.getApplicationContext<android.content.Context>()
                            .noBackupFilesDir
                            .resolve("plugin-setup-drafts")
                            .listFiles()
                            .orEmpty()
                            .filter { it.isFile && it.name.endsWith(".json") }
                    assertEquals(
                        "Only the active editor may save ordinary inputs",
                        1,
                        snapshots.size,
                    )
                }
                when (recovery) {
                    "missing" ->
                        PluginSettingsDraftStore.clear(ApplicationProvider.getApplicationContext())
                    "changed" -> current.set(initial.copy(revision = "r2", name = "Remote change"))
                    "deleted" -> current.set(null)
                }
                restoring.set(true)
                restoration.emulateSavedInstanceStateRestore()
                compose.onNodeWithText("Nom de la route").assertExists()
                if (recovery == "missing") {
                    compose
                        .onNodeWithText(
                            "Les saisies de la route n’ont pas pu être récupérées. Annulez la modification puis recommencez depuis la route enregistrée."
                        )
                        .assertExists()
                    compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
                    compose.waitUntil(10000) { restoredRead.count == 0L }
                    compose.onNodeWithText("Annuler la modification").assertIsNotEnabled()
                    // Restoration revalidates asynchronously. A click while the
                    // reload is pending is correctly ignored by the disabled button.
                    releaseRestoredRead.countDown()
                    waitEnabled("Annuler la modification")
                    compose
                        .onNodeWithText("Annuler la modification")
                        .performScrollTo()
                        .performClick()
                    waitEnabled("Modifier la route Existing route")
                    compose
                        .onNodeWithText("Modifier la route Existing route")
                        .performScrollTo()
                        .performClick()
                    compose.onNodeWithText("Nom de la route").assertTextContains("Existing route")
                    compose
                        .onNodeWithText("Valeur de la condition 1")
                        .assertTextContains("original")
                    compose
                        .onNodeWithText("Identifiants des personnes autorisées")
                        .assertTextContains(users.joinToString("\n"))
                    assertTrue(
                        "Losing local inputs must never save a broader route",
                        writes.isEmpty(),
                    )
                    return
                }
                if (recovery == "changed" || recovery == "deleted") {
                    compose.waitUntil(10000) {
                        compose
                            .onAllNodesWithText(
                                "La route a changé. Fermez cet éditeur puis actualisez les routes."
                            )
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
                    assertTrue(
                        "An obsolete edit must not become a fresh route creation",
                        writes.isEmpty(),
                    )
                    return
                }
                compose.onNodeWithText("Nom de la route").assertTextContains("Draft route")
                waitEnabled("Enregistrer")
                compose.onNodeWithText("Valeur de la condition 1").assertTextContains("support")
                compose
                    .onNodeWithText("Identifiants des personnes autorisées")
                    .assertTextContains(users.dropLast(1).joinToString("\n"))
                assertTrue("Restoration must not submit a route", writes.isEmpty())
                compose.onNodeWithText("Enregistrer").performClick()
                compose.waitUntil(10000) {
                    writes.size == 1 &&
                        compose
                            .onAllNodesWithText("Modifier la route Existing route")
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                }
                val (target, body) = writes.single()
                assertTrue(target.endsWith("$root/routes/route"))
                assertEquals("r1", body["revision"]!!.jsonPrimitive.content)
                assertEquals("Draft route", body["name"]!!.jsonPrimitive.content)
                assertEquals(
                    "support",
                    body["match"]!!.jsonObject["channel"]!!.jsonPrimitive.content,
                )
                assertEquals(
                    users.dropLast(1),
                    wireJson.decodeFromJsonElement<List<String>>(body["userIds"]!!),
                )
                assertEquals(
                    initial.people.dropLast(1),
                    wireJson.decodeFromJsonElement<List<PluginLabel>>(body["people"]!!),
                )
            } finally {
                releaseRestoredRead.countDown()
                models.clear()
            }
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
                                        setupDrafts = true,
                                        packages =
                                            listOf(
                                                PluginEntry(
                                                    id = manifest.id,
                                                    active = "1",
                                                    manifest = manifest,
                                                )
                                            ),
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

    @Test fun activeRemoteInstanceChoicesAreBoundToTheEditor() = remoteInstanceChoices(false)

    @Test fun preparedRemoteInstanceChoicesAreBoundToTheEditor() = remoteInstanceChoices(true)

    private fun remoteInstanceChoices(prepared: Boolean) {
        val manifest =
            PluginManifest(
                id = "remote-root",
                name = "Remote service",
                version = "1",
                instanceForm = true,
                settings =
                    listOf(
                        PluginSetting(
                            "channel",
                            "Channel",
                            type = "select",
                            dynamic = true,
                            remote = true,
                        )
                    ),
            )
        val connection =
            PluginConnection(
                id = "app",
                pluginId = manifest.id,
                name = "Remote app",
                settings = buildJsonObject { put("channel", "saved-off-page") },
            )
        val draft =
            PluginSetupDraft(
                id = "app",
                pluginId = manifest.id,
                name = connection.name,
                revision = "r1",
                settings = connection.settings,
                expiresAt = System.currentTimeMillis() + 86400000,
            )
        val previews = CopyOnWriteArrayList<JsonObject>()
        val writes = CopyOnWriteArrayList<JsonObject>()
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val released = CountDownLatch(1)
        fun waitText(text: String) =
            compose.waitUntil(10000) {
                compose.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty()
            }
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("/plugins") ->
                                wireJson.encodeToString(
                                    PluginCatalog(
                                        setupDrafts = true,
                                        packages =
                                            listOf(
                                                PluginEntry(
                                                    id = manifest.id,
                                                    active = "1",
                                                    manifest = manifest,
                                                )
                                            ),
                                        connections =
                                            if (prepared) emptyList() else listOf(connection),
                                    )
                                )
                            path.endsWith("/setup-drafts/app") -> wireJson.encodeToString(draft)
                            else -> null
                        }
                    }
                ) { path, body ->
                    if (!path.contains("/remote-root/")) return@dispatcher null
                    val input = wireJson.parseToJsonElement(body).jsonObject
                    if (!path.endsWith("/form")) {
                        writes += input
                        return@dispatcher if (prepared)
                            wireJson.encodeToString(
                                draft.copy(
                                    revision = "r2",
                                    settings = input.getValue("settings").jsonObject,
                                )
                            )
                        else
                            wireJson.encodeToString(
                                connection.copy(settings = input.getValue("settings").jsonObject)
                            )
                    }
                    previews += input
                    val lookup =
                        input["lookup"]?.let {
                            wireJson.decodeFromJsonElement<PluginFormLookup>(it)
                        } ?: return@dispatcher "null"
                    if (lookup.query == "old") {
                        entered.countDown()
                        check(release.await(20, TimeUnit.SECONDS))
                        released.countDown()
                    }
                    val options =
                        when {
                            lookup.query == "unknown" || lookup.cursor == "empty" -> emptyList()
                            lookup.query.isNotEmpty() ->
                                listOf(PluginOption(lookup.query, "${lookup.query} choice"))
                            lookup.cursor == "last" -> listOf(PluginOption("C2", "Engineering"))
                            else -> listOf(PluginOption("C1", "General"))
                        }
                    wireJson.encodeToString(
                        PluginForm(
                            options = mapOf("channel" to options),
                            lookup = lookup,
                            pages =
                                if (lookup.query == "unknown") null
                                else
                                    mapOf(
                                        "channel" to
                                            PluginChoicePage(
                                                when (lookup.cursor) {
                                                    null -> "empty"
                                                    "empty" -> "last"
                                                    else -> null
                                                }
                                            )
                                    ),
                        )
                    )
                }
            server.start()
            val vm =
                show(server) { model, state ->
                    if (prepared) PluginSetup(model, state, manifest, draft, {}, {}, {})
                    else PluginScreen(model, state, manifest.id) {}
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                if (!prepared) {
                    waitText("Modifier")
                    compose.onNodeWithText("Modifier").performScrollTo().performClick()
                }
                waitText("General")
                compose.onNodeWithContentDescription("General").performScrollTo().performClick()
                waitText("Page suivante")
                compose.onNodeWithText("Page suivante").performScrollTo().performClick()
                waitText("0 choix sur cette page · D’autres choix sont disponibles")
                compose.onNodeWithText("General").assertExists()
                compose.onNodeWithText("Page suivante").performScrollTo().performClick()
                waitText("1 choix sur cette page · Fin des résultats")
                compose.onNodeWithText("General").assertExists()
                compose
                    .onNodeWithText("Rechercher · Channel")
                    .performScrollTo()
                    .performTextReplacement("unknown")
                waitText("0 choix sur cette page · Complétude inconnue")
                compose.waitUntil(10000) {
                    previews.any {
                        it["lookup"]?.jsonObject?.get("query") == JsonPrimitive("unknown")
                    } &&
                        compose
                            .onAllNodesWithText("Chargement des choix…")
                            .fetchSemanticsNodes()
                            .isEmpty()
                }
                compose.onNodeWithText("Rechercher · Channel").performTextReplacement("old")
                compose.waitUntil(10000) { entered.count == 0L }
                compose.onNodeWithText("Rechercher · Channel").performTextReplacement("new")
                waitText("new choice")
                release.countDown()
                compose.waitUntil(10000) { released.count == 0L }
                compose.onNodeWithText("old choice").assertDoesNotExist()
                val save = if (prepared) "Enregistrer la configuration" else "Enregistrer"
                compose.waitUntil(10000) {
                    compose
                        .onAllNodes(hasText(save) and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                val saveButton = compose.onNodeWithText(save)
                if (prepared) saveButton.performScrollTo()
                saveButton.performClick()
                compose.waitUntil(10000) { writes.isNotEmpty() }
                assertEquals(
                    "C1",
                    writes.last()["settings"]?.jsonObject?.get("channel")?.jsonPrimitive?.content,
                )
                val lookups = previews.filter { it["lookup"] != null }
                assertTrue(lookups.isNotEmpty())
                assertTrue(
                    lookups.all {
                        if (prepared) it["revision"] == JsonPrimitive("r1")
                        else it["instance"] == JsonPrimitive("app")
                    }
                )
                assertTrue(lookups.none { it.containsKey("changed") })
            } finally {
                release.countDown()
                models.clear()
            }
        }
    }

    @Test
    fun remoteMultipleChoicesKeepOffPageIdsAndAllowRemoval() {
        val field =
            PluginSetting("channel", "Members", type = "multiselect", dynamic = true, remote = true)
        val current = AtomicReference<String>()
        val requests = CopyOnWriteArrayList<JsonObject>()
        fun waitText(text: String) =
            compose.waitUntil(10000) {
                compose.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty()
            }
        MockWebServer().use { server ->
            server.dispatcher = dispatcher { path, body ->
                if (!path.endsWith("/plugins/remote-multi/form")) return@dispatcher null
                val input = wireJson.parseToJsonElement(body).jsonObject
                requests += input
                val lookup =
                    wireJson.decodeFromJsonElement<PluginFormLookup>(input.getValue("lookup"))
                wireJson.encodeToString(
                    PluginForm(
                        lookup = lookup,
                        options =
                            mapOf(
                                "channel" to
                                    if (lookup.cursor == "empty") emptyList()
                                    else listOf(PluginOption("M1", "General member"))
                            ),
                        pages =
                            mapOf(
                                "channel" to
                                    PluginChoicePage(
                                        when (lookup.cursor) {
                                            null -> "empty"
                                            "empty" -> "last"
                                            else -> null
                                        }
                                    )
                            ),
                    )
                )
            }
            server.start()
            val vm =
                show(server) { model, _ ->
                    var value by remember { mutableStateOf("[\"M0\"]") }
                    val values = mapOf("channel" to value)
                    val lookup =
                        PluginChoiceLookup(
                            model.api,
                            "/plugins/remote-multi/form",
                            buildJsonObject {
                                put("settings", pluginSettings(listOf(field), values))
                            },
                        )
                    Page {
                        PluginFields(
                            listOf(field),
                            values,
                            { _, next -> value = next },
                            emptyMap(),
                            { _, _ -> },
                            lookup = lookup,
                        )
                    }
                    SideEffect { current.set(value) }
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                waitText("General member")
                compose
                    .onNodeWithContentDescription("General member")
                    .performScrollTo()
                    .performClick()
                compose.waitUntil(10000) { current.get() == "[\"M0\",\"M1\"]" }
                compose.waitUntil(10000) {
                    compose
                        .onAllNodes(hasText("Page suivante") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Page suivante").performScrollTo().performClick()
                waitText("0 choix sur cette page · D’autres choix sont disponibles")
                compose.onNodeWithText("Retirer General member").performScrollTo().performClick()
                compose.waitUntil(10000) { current.get() == "[\"M0\"]" }
                assertTrue(
                    requests.any {
                        it["settings"]?.jsonObject?.get("channel") ==
                            buildJsonArray {
                                add("M0")
                                add("M1")
                            }
                    }
                )
            } finally {
                models.clear()
            }
        }
    }

    @Test
    fun activeResourceFormsDiscardStaleParentsAndRestoreWithoutSecrets() = scopedResourceForm(false)

    @Test
    fun preparedResourceFormsDiscardStaleParentsAndRestoreWithoutSecrets() =
        scopedResourceForm(true)

    @Test
    fun activeRemoteResourceChoicesRetainSelectionAcrossPages() = scopedResourceForm(false, true)

    @Test
    fun preparedRemoteResourceChoicesRetainSelectionAcrossPages() = scopedResourceForm(true, true)

    private fun scopedResourceForm(prepared: Boolean, remote: Boolean = false) {
        val workspaces = PluginResourceCollection("workspace", "Workspaces", "Workspace")
        val channels =
            PluginResourceCollection(
                name = "channel",
                title = "Channels",
                singular = "Channel",
                parent = "workspace",
                form = true,
                settings =
                    listOf(
                        PluginSetting(
                            "channel",
                            "Channel",
                            type = "select",
                            dynamic = true,
                            remote = remote,
                            required = true,
                        ),
                        PluginSetting("verified", "Verified name", hidden = true),
                        PluginSetting("token", "Credential", type = "secret"),
                    ),
            )
        val manifest = PluginManifest(id = "scoped-form", resources = listOf(workspaces, channels))
        val location =
            PluginResourceLocation(manifest.id, "app", if (prepared) "setup-r1" else null)
        val engineering =
            PluginResource(
                id = "engineering",
                connectionId = "app",
                collection = "workspace",
                name = "Engineering",
                revision = "e1",
            )
        val sales = engineering.copy(id = "sales", name = "Sales", revision = "s1")
        val previews = CopyOnWriteArrayList<JsonObject>()
        val writes = CopyOnWriteArrayList<JsonObject>()
        val stored = AtomicReference<PluginResource?>(null)
        val hold = java.util.concurrent.atomic.AtomicBoolean(true)
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val released = CountDownLatch(1)
        val restoration = StateRestorationTester(compose)
        fun waitEnabled(label: String) =
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText(label) and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    read = { path ->
                        when {
                            path.endsWith("${location.path}/workspace") ->
                                wireJson.encodeToString(PluginPage(listOf(engineering, sales)))
                            path.endsWith("${location.path}/channel") ->
                                wireJson.encodeToString(PluginPage(listOfNotNull(stored.get())))
                            else -> null
                        }
                    }
                ) { path, body ->
                    if (!path.contains(location.path)) return@dispatcher null
                    val input = wireJson.parseToJsonElement(body).jsonObject
                    if (path.endsWith("/channel/form")) {
                        previews += input
                        val parent = input["parentId"]?.jsonPrimitive?.content
                        if (parent == "engineering" && hold.compareAndSet(true, false)) {
                            entered.countDown()
                            check(release.await(20, TimeUnit.SECONDS))
                            released.countDown()
                        }
                        val lookup =
                            input["lookup"]?.let {
                                wireJson.decodeFromJsonElement<PluginFormLookup>(it)
                            }
                        if (lookup != null) {
                            val choices =
                                if (parent == null || lookup.cursor == "empty") emptyList()
                                else listOf(PluginOption("$parent-general", "$parent general"))
                            return@dispatcher wireJson.encodeToString(
                                PluginForm(
                                    options = mapOf("channel" to choices),
                                    lookup = lookup,
                                    pages =
                                        mapOf(
                                            "channel" to
                                                PluginChoicePage(
                                                    when (lookup.cursor) {
                                                        null -> "empty"
                                                        "empty" -> "last"
                                                        else -> null
                                                    }
                                                )
                                        ),
                                )
                            )
                        }
                        if (parent == null)
                            """{"errors":{"parentId":"Choose a workspace first."}}"""
                        else
                            wireJson.encodeToString(
                                PluginForm(
                                    options =
                                        mapOf(
                                            "channel" to
                                                listOf(
                                                    PluginOption(
                                                        "$parent-general",
                                                        "$parent general",
                                                    )
                                                )
                                        ),
                                    values =
                                        buildJsonObject { put("verified", "$parent verified") },
                                    view =
                                        buildJsonArray {
                                            add(
                                                buildJsonObject {
                                                    put("type", "alert")
                                                    put("tone", "info")
                                                    put("text", "Only $parent is queried.")
                                                }
                                            )
                                        },
                                )
                            )
                    } else if (path.endsWith("/channel")) {
                        writes += input
                        val resource =
                            PluginResource(
                                id = "saved",
                                connectionId = "app",
                                collection = "channel",
                                parentId = input["parentId"]!!.jsonPrimitive.content,
                                name = input["name"]!!.jsonPrimitive.content,
                                revision = "c1",
                                settings = input["settings"]!!.jsonObject,
                                secretKeys = listOf("token"),
                            )
                        stored.set(resource)
                        wireJson.encodeToString(resource)
                    } else null
                }
            server.start()
            val vm =
                show(server, restoration) { model, state ->
                    Page { PluginResources(model, state, manifest, location) }
                }
            val models = ViewModelStore().apply { put("fixture", vm) }
            try {
                waitEnabled("Ajouter · Channel")
                compose.onNodeWithText("Ajouter · Channel").performScrollTo().performClick()
                waitEnabled("Nom · Channel")
                compose
                    .onNodeWithText("Nom · Channel")
                    .performScrollTo()
                    .performTextInput("Support")
                compose.onNodeWithText("Choisir…").performScrollTo().performClick()
                compose.onNode(hasText("Engineering") and hasClickAction()).performClick()
                compose.waitUntil(10000) { entered.count == 0L }
                compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
                compose
                    .onNode(hasText("Engineering") and hasClickAction())
                    .performScrollTo()
                    .performClick()
                compose.onNode(hasText("Sales") and hasClickAction()).performClick()
                waitEnabled("sales general")
                compose
                    .onNodeWithContentDescription("sales general")
                    .performScrollTo()
                    .performClick()
                waitEnabled("Enregistrer")
                release.countDown()
                compose.waitUntil(10000) { released.count == 0L }
                compose.waitForIdle()
                compose.onNodeWithText("Only sales is queried.").assertExists()
                compose.onNodeWithText("engineering general").assertDoesNotExist()
                if (remote) {
                    waitEnabled("Page suivante")
                    compose.onNodeWithText("Page suivante").performScrollTo().performClick()
                    compose.waitUntil(10000) {
                        compose
                            .onAllNodesWithText(
                                "0 choix sur cette page · D’autres choix sont disponibles"
                            )
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    compose.onNodeWithText("sales general").assertExists()
                    compose.onNodeWithText("Page suivante").performScrollTo().performClick()
                    compose.waitUntil(10000) {
                        compose
                            .onAllNodesWithText("1 choix sur cette page · Fin des résultats")
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    compose.onNodeWithText("Page suivante").assertDoesNotExist()
                    val selected =
                        previews
                            .last()["settings"]
                            ?.jsonObject
                            ?.get("channel")
                            ?.jsonPrimitive
                            ?.content
                    assertEquals("sales-general", selected)
                }
                compose
                    .onNodeWithText("Credential")
                    .performScrollTo()
                    .performTextInput("fixture-secret")
                waitEnabled("Enregistrer")
                val before = previews.size
                restoration.emulateSavedInstanceStateRestore()
                waitEnabled("Enregistrer")
                compose.waitUntil(10000) { previews.size > before }
                compose
                    .onNodeWithText("Nom · Channel")
                    .performScrollTo()
                    .assertTextContains("Support")
                compose
                    .onNodeWithText("Credential")
                    .performScrollTo()
                    .assertTextEquals("Credential", "")
                assertEquals(JsonObject(emptyMap()), previews.last()["secrets"])
                assertEquals("sales", previews.last()["parentId"]?.jsonPrimitive?.content)
                assertEquals(
                    "sales-general",
                    previews.last()["settings"]?.jsonObject?.get("channel")?.jsonPrimitive?.content,
                )
                assertTrue(writes.isEmpty())
                compose.onNodeWithText("Credential").performTextInput("replacement-secret")
                waitEnabled("Enregistrer")
                capture(if (prepared) "prepared-resource-form" else "active-resource-form")
                compose.onNodeWithText("Enregistrer").performClick()
                waitEnabled("Modifier Support")
                assertEquals(1, writes.size)
                assertEquals(
                    "sales verified",
                    writes
                        .single()["settings"]
                        ?.jsonObject
                        ?.get("verified")
                        ?.jsonPrimitive
                        ?.content,
                )
                assertEquals(
                    "replacement-secret",
                    writes.single()["secrets"]?.jsonObject?.get("token")?.jsonPrimitive?.content,
                )
                assertEquals(
                    if (prepared) JsonPrimitive("setup-r1") else null,
                    writes.single()["setupRevision"],
                )
                compose.onNodeWithText("Modifier Support").performScrollTo().performClick()
                waitEnabled("Enregistrer")
                compose.waitUntil(10000) {
                    previews.last()["resource"]?.jsonPrimitive?.content == "saved"
                }
                assertEquals("c1", previews.last()["revision"]?.jsonPrimitive?.content)
                assertEquals(JsonObject(emptyMap()), previews.last()["secrets"])
                compose.onNodeWithContentDescription("Fermer").performClick()
                assertEquals(1, writes.size)
            } finally {
                release.countDown()
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
                                        setupDrafts = true,
                                        packages =
                                            listOf(
                                                PluginEntry(
                                                    id = manifest.id,
                                                    active = "1.0.0",
                                                    manifest = manifest,
                                                )
                                            ),
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

    @Test
    fun anAgentManagesScopedRightsWithoutWholeInstanceAccess() {
        val entry =
            wireJson.decodeFromString<PluginEntry>(
                """{
            "id":"scoped-rights","active":"1","status":"active","manifest":{
            "id":"scoped-rights","name":"Resource service","agentAccess":{},
            "resources":[{"name":"workspace","title":"Workspaces","singular":"Workspace"}],
            "operations":[{"name":"read-messages","collection":"workspace","profile":"read","scopes":["channel"]},
            {"name":"send-message","collection":"workspace","profile":"contribute","scopes":["channel"]}]}}
        """
            )
        val scoped =
            PluginCatalog(
                setupDrafts = true,
                packages =
                    listOf(
                        entry,
                        entry.copy(
                            id = "other-rights",
                            manifest = entry.manifest?.copy(id = "other-rights"),
                        ),
                    ),
                connections =
                    listOf(
                        PluginConnection(
                            id = "app",
                            pluginId = "scoped-rights",
                            name = "Application",
                        ),
                        PluginConnection(
                            id = "app-other",
                            pluginId = "scoped-rights",
                            name = "Application",
                        ),
                        PluginConnection(
                            id = "other-app",
                            pluginId = "other-rights",
                            name = "Application",
                        ),
                    ),
                resourceGrantCounts = mapOf("helper" to 1),
            )
        val rules =
            CopyOnWriteArrayList<PluginResourceGrant>().apply {
                add(
                    PluginResourceGrant(
                        id = "r1",
                        resourceId = "team",
                        agentId = "helper",
                        profile = PluginAccessProfile.Read,
                        scopes = mapOf("channel" to listOf("C111")),
                    )
                )
            }
        val writes = CopyOnWriteArrayList<JsonObject>()
        val reads = CopyOnWriteArrayList<String>()
        val root =
            "/api/installations/fixture/api/plugins/scoped-rights/connections/app/resources/workspace"
        val directory = "/api/installations/fixture/api/agents/helper/plugin-resource-grants"
        MockWebServer().use { server ->
            server.dispatcher =
                dispatcher(
                    includeGet = true,
                    read = { path ->
                        reads += path
                        if (path == "/api/installations/fixture/api/plugins/packages/scoped-rights")
                            wireJson.encodeToString(entry.configuration())
                        else null
                    },
                ) { path, body ->
                    when (path.substringBefore('?')) {
                        directory -> buildJsonObject {
                                put(
                                    "items",
                                    JsonArray(
                                        rules.map { rule ->
                                            buildJsonObject {
                                                wireJson
                                                    .encodeToJsonElement(rule)
                                                    .jsonObject
                                                    .forEach { (key, value) -> put(key, value) }
                                                put("connectionId", "app")
                                                put("pluginId", "scoped-rights")
                                                put("collection", "workspace")
                                                put("instanceName", "Application")
                                                put("resourceName", "Engineering")
                                            }
                                        }
                                    ),
                                )
                            }
                                .toString()
                        "$directory/r1" -> {
                            rules.clear()
                            "{}"
                        }
                        root.replace("/resources/", "/resource-choices/") ->
                            """{"items":[{"id":"team","name":"Engineering","collection":"workspace"}]}"""
                        "$root/team/grants" -> {
                            if (body.isNotBlank()) {
                                val input = wireJson.parseToJsonElement(body).jsonObject
                                writes += input
                                rules +=
                                    wireJson
                                        .decodeFromJsonElement<PluginResourceGrant>(input)
                                        .copy(id = "r2", resourceId = "team")
                                "{}"
                            } else
                                buildJsonObject {
                                    put("items", wireJson.encodeToJsonElement(rules.toList()))
                                }
                                    .toString()
                        }
                        else -> null
                    }
                }
            val delegate = server.dispatcher
            val rejectSchema = java.util.concurrent.atomic.AtomicBoolean(true)
            val choicesRequested = java.util.concurrent.atomic.AtomicBoolean(false)
            val releaseChoices = java.util.concurrent.CountDownLatch(1)
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        if (
                            request.path ==
                                "/api/installations/fixture/api/plugins/packages/scoped-rights" &&
                                rejectSchema.getAndSet(false)
                        ) {
                            reads += request.path.orEmpty()
                            return MockResponse()
                                .setResponseCode(503)
                                .setBody("""{"error":"Schema temporarily unavailable"}""")
                        }
                        if (request.path == root.replace("/resources/", "/resource-choices/")) {
                            choicesRequested.set(true)
                            if (!releaseChoices.await(10, java.util.concurrent.TimeUnit.SECONDS)) {
                                return MockResponse().setResponseCode(503)
                            }
                        }
                        return delegate.dispatch(request)
                    }
                }
            server.start()
            show(server) { vm, state ->
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    AgentResourceAccess(
                        vm,
                        state,
                        "helper",
                        scoped.agentDirectory().copy(legacyPackages = null),
                        changed = {},
                    )
                }
            }
            compose.waitUntil(15000) {
                compose.onAllNodesWithText("channel : C111").fetchSemanticsNodes().isNotEmpty()
            }
            compose
                .onNodeWithText("Révoquer le droit sur la ressource")
                .performScrollTo()
                .performClick()
            compose.onNodeWithText("Confirmer la révocation").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Aucune règle enregistrée sur les ressources.")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onAllNodesWithText("Choisir…").onFirst().performScrollTo().performClick()
            capture("agent-resource-instance-choices")
            compose
                .onNodeWithText("Application · app · scoped-rights (Resource service)")
                .performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Réessayer le schéma des droits")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Collection de ressources").assertDoesNotExist()
            compose
                .onNodeWithText("Réessayer le schéma des droits")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Collection de ressources")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onAllNodesWithText("Choisir…").onFirst().performScrollTo().performClick()
            compose.onNodeWithText("Workspaces").performClick()
            compose.waitUntil(10000) { choicesRequested.get() }
            // A visible field is not an available choice while its request is pending.
            compose.onNodeWithText("Choisir…").assertIsNotEnabled()
            releaseChoices.countDown()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Choisir…") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Choisir…").performScrollTo().performClick()
            compose.onNodeWithText("Engineering").performClick()
            compose.onNodeWithText("Accès des agents").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Ajouter une règle").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Ajouter une règle").performScrollTo().performClick()
            compose.onNodeWithText("Agent", substring = false).assertDoesNotExist()
            compose.onNodeWithContentDescription("Contribution").performScrollTo().performClick()
            compose.onNodeWithText("Limiter channel").performScrollTo().performTextInput("C222")
            compose.onNodeWithText("Enregistrer").assertIsEnabled().performClick()
            compose.waitUntil(10000) { writes.size == 1 }
            assertEquals("helper", writes.single()["agentId"]?.jsonPrimitive?.content)
            assertEquals("contribute", writes.single()["profile"]?.jsonPrimitive?.content)
            assertEquals(
                listOf("C222"),
                writes.single()["scopes"]?.jsonObject?.get("channel")?.jsonArray?.map {
                    it.jsonPrimitive.content
                },
            )
            assertTrue(scoped.connections.all { it.agentIds.isEmpty() })
            assertTrue(
                reads.contains("/api/installations/fixture/api/plugins/packages/scoped-rights")
            )
            assertTrue(reads.none { it.endsWith("/plugins/packages/other-rights") })
            assertTrue(reads.any { it.contains("/resource-choices/workspace") })
            capture("agent-resource-permissions")
        }
    }

    @Test
    fun anAgentAlreadyUsingAnotherGitHubAccountCannotBeGrantedASecond() {
        // Runs receive the account's git credentials: one account per agent.
        val github =
            forge.copy(
                manifest =
                    requireNotNull(forge.manifest)
                        .copy(
                            agentAccess = PluginAgentAccess(),
                            runCredentials = buildJsonObject { putJsonArray("git") {} },
                        )
            )
        val work =
            PluginConnection(
                id = "work",
                pluginId = "forge",
                name = "Work",
                agentIds = listOf("helper"),
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
            assertEquals(JsonArray(listOf(JsonPrimitive("helper"))), payload["agentIds"])
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
    @Test
    fun selectedPackageReadsKeepCurrentGrantsWithoutReadingTheGlobalCatalogue() =
        editExistingAccount(older = true, bounded = true)

    @Test
    fun selectedPackageExactReadDoesNotRestoreARevokedGrant() =
        editExistingAccount(older = true, revokedMeanwhile = true, bounded = true)

    private fun editExistingAccount(
        older: Boolean,
        revokedMeanwhile: Boolean = false,
        bounded: Boolean = false,
    ) {
        MockWebServer().use { server ->
            val saved = CopyOnWriteArrayList<String>()
            val editing = java.util.concurrent.atomic.AtomicBoolean(false)
            server.dispatcher =
                dispatcher(
                    includeGet = true,
                    current = !older,
                    read = { path ->
                        if (!bounded) null
                        else
                            when {
                                path.endsWith("/plugins/packages/onepassword") ->
                                    wireJson.encodeToString(
                                        onePassword
                                            .configuration()
                                            .copy(
                                                instanceReads = true,
                                                accessReads = true,
                                                capabilities = emptyList(),
                                            )
                                    )
                                path.endsWith("/plugins/packages/onepassword/connections") ->
                                    wireJson.encodeToString(PluginPage(listOf(account), "op1"))
                                path.endsWith(
                                    "/plugins/packages/onepassword/connections?cursor=op1"
                                ) ->
                                    wireJson.encodeToString(
                                        PluginPage<PluginConnection>(emptyList())
                                    )
                                path.endsWith("/plugins/instance-access") ->
                                    """{"items":[],"nextCursor":"next"}"""
                                path.endsWith("/plugins/instance-access?cursor=next") ->
                                    wireJson.encodeToString(
                                        PluginPage(
                                            listOf(
                                                PluginInstanceAccess(
                                                    "another",
                                                    "notes",
                                                    "Notes",
                                                    "Other team",
                                                    false,
                                                    listOf("helper"),
                                                    listOf("resource-agent"),
                                                )
                                            )
                                        )
                                    )
                                path.endsWith("/plugins/packages/onepassword/connections/op1") ->
                                    wireJson.encodeToString(
                                        if (revokedMeanwhile && editing.get())
                                            account.copy(agentIds = emptyList())
                                        else account
                                    )
                                path.endsWith("/api/plugins") ->
                                    error("Detail must never read the global catalogue")
                                else -> null
                            }
                    },
                ) { path, body ->
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
                                    setupDrafts = !older,
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
            if (older) {
                val paths = List(server.requestCount) { server.takeRequest().path.orEmpty() }
                assertFalse(paths.any { "/setup-drafts" in it })
            }
            val payload = wireJson.parseToJsonElement(saved.single()).jsonObject
            if (revokedMeanwhile) assertFalse("agentIds" in payload)
            else if (older)
                assertEquals(JsonArray(listOf(JsonPrimitive(MAIN_AGENT_ID))), payload["agentIds"])
            else assertFalse("agentIds" in payload)
        }
    }

    private fun dispatcher(
        includeGet: Boolean = false,
        read: (String) -> String? = { null },
        current: Boolean = true,
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
                if (request.method == "GET")
                    read(path)?.let {
                        return MockResponse().setBody(it)
                    }
                if (
                    request.method == "GET" &&
                        path.contains("/api/plugins/packages") &&
                        !path.contains("/api/plugins/packages/operations/")
                )
                    return MockResponse().setResponseCode(404)
                if (request.method == "GET" && path.endsWith("/setup-drafts"))
                    return if (current) MockResponse().setBody("""{"items":[]}""")
                    else MockResponse().setResponseCode(404)
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
                                    setupDrafts = current,
                                    packages = packages,
                                    connections = connections,
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
    private fun openSharedResourceAuthorization(index: Int = 0) {
        compose.waitUntil(10000) {
            compose
                .onAllNodes(hasText("Gérer l’autorisation") and isEnabled())
                .fetchSemanticsNodes()
                .size > index
        }
        compose
            .onAllNodes(hasText("Gérer l’autorisation") and isEnabled())[index]
            .performScrollTo()
            .performClick()
        compose.waitForIdle()
    }

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
