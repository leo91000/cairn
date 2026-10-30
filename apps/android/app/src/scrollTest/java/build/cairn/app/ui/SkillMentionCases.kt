package build.cairn.app.ui

import android.app.Application
import androidx.activity.OnBackPressedDispatcher
import androidx.activity.compose.LocalOnBackPressedDispatcherOwner
import androidx.compose.runtime.*
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.*
import org.junit.Assert.*

abstract class SkillMentionCases {
    @get:Rule val compose = createComposeRule()

    @Test fun dollarSuggestsSkillsAndInsertsTheChosenOneBeforeSending() = skillJourney(false)

    @Test fun emptySkillsExplainsTheMissingSuggestionsAndCanBeDismissed() = skillJourney(true)

    @Test
    fun emptySkillsCanBeUsedAfterASlowInstallationRefresh() =
        skillJourney(noSkills = true, slowRefresh = true)

    private fun skillJourney(noSkills: Boolean, slowRefresh: Boolean = false) {
        MockWebServer().use { server ->
            val sent = CopyOnWriteArrayList<JsonObject>()
            val agent =
                Agent(
                    exclusiveNodes = emptyList(),
                    plugins = emptyList(),
                    revision = "",
                    createdAt = 0,
                    access =
                        AccessPolicy(
                            mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                            projects = null,
                            skills = null,
                            mcps = null,
                        ),
                    id = MAIN_AGENT_ID,
                    name = "Agent principal",
                )
            val run =
                Run(
                    id = "run",
                    status = "succeeded",
                    snapshot =
                        Snapshot(
                            agent = agent,
                            task =
                                build.cairn.contracts.RunTaskSnapshot(
                                    agentId = build.cairn.app.data.MAIN_AGENT_ID,
                                    id = "00000000-0000-4000-8000-000000000003",
                                    name = "Fixture",
                                    prompt = "Fixture",
                                ),
                        ),
                    taskId = "",
                )
            val chat =
                Chat(
                    id = "6cc6efab-10af-54c7-a91b-dcff664166a1",
                    title = "Skills",
                    agentId = agent.id,
                    runId = "run",
                    agentName = agent.name,
                    run = run,
                )
            val skills =
                if (noSkills) emptyList()
                else
                    listOf(
                        Skill(
                            name = "review",
                            description = "Relire les changements en cours",
                            usageCount = 3,
                        ),
                        Skill(name = "deploy", description = "Publier une version"),
                        Skill(name = "broken", description = "Invalide", valid = false),
                    )
            val history =
                listOf(
                    RunEvent(
                        id = 1,
                        createdAt = 1789315200000,
                        type = "chat.user",
                        text = "Merci d’utiliser \$review ici",
                    )
                )
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (
                            path ==
                                "/api/installations/fixture/api/chats/6cc6efab-10af-54c7-a91b-dcff664166a1/stream"
                        ) {
                            val frame =
                                "event: batch\nid: 1\ndata: ${wireJson.encodeToString(LiveBatch(history, LiveState(chat = chat, run = run), true, false))}\n\n"
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(frame + ": keepalive\n\n".repeat(10000))
                                .throttleBody(
                                    frame.toByteArray().size.toLong(),
                                    1,
                                    java.util.concurrent.TimeUnit.SECONDS,
                                )
                        }
                        val body =
                            when (path) {
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/account/session" -> beaconAccountFixture("fixture")
                                "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(
                                        build.cairn.contracts.ListAgentsOutput(listOf(agent))
                                    )
                                "/api/installations/fixture/api/skills" ->
                                    wireJson.encodeToString(skills)
                                "/api/installations/fixture/api/chats/6cc6efab-10af-54c7-a91b-dcff664166a1/messages" -> {
                                    sent +=
                                        wireJson
                                            .parseToJsonElement(request.body.readUtf8())
                                            .jsonObject
                                    wireJson.encodeToString(
                                        ChatMessage(
                                            id = sent.last().getValue("id").jsonPrimitive.content
                                        )
                                    )
                                }
                                "/api/installations/fixture/api/codex/models" -> """{"models":[]}"""
                                "/api/installations/fixture/api/overview" ->
                                    installationOverviewFixture()
                                else ->
                                    if (
                                        request.requestUrl
                                            ?.encodedPath
                                            ?.endsWith("/api/projects") == true
                                    )
                                        """{"projects":[]}"""
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/agents") ==
                                            true
                                    )
                                        """{"agents":[]}"""
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/tasks") ==
                                            true
                                    )
                                        """{"tasks":[]}"""
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/mcps") ==
                                            true
                                    )
                                        """{"mcps":[]}"""
                                    else "[]"
                            }
                        return MockResponse().setBody(body).apply {
                            if (slowRefresh && path.endsWith("/skills"))
                                setBodyDelay(750, java.util.concurrent.TimeUnit.MILLISECONDS)
                        }
                    }
                }
            server.start()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    SkillVault(),
                    beaconOrigin = "",
                )
            val models = ViewModelStore().apply { put("fixture", vm) }
            var mounted by mutableStateOf(true)
            var backDispatcher: OnBackPressedDispatcher? = null
            try {
                compose.setContent {
                    if (mounted) {
                        backDispatcher =
                            LocalOnBackPressedDispatcherOwner.current?.onBackPressedDispatcher
                        val state by vm.state.collectAsStateWithLifecycle()
                        LaunchedEffect(Unit) {
                            vm.state.first { it.ready }
                            if (!vm.state.value.session.authenticated)
                                vm.connect(server.url("/").toString())
                        }
                        CairnTheme {
                            if (state.session.authenticated)
                                ChatScreen(
                                    vm,
                                    state,
                                    "6cc6efab-10af-54c7-a91b-dcff664166a1",
                                    openChat = {},
                                    openRun = {},
                                )
                        }
                    }
                }
                compose.waitUntil(20000) {
                    compose
                        .onAllNodesWithText(
                            if (noSkills) "Votre message…" else "Votre message… $ pour les skills"
                        )
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                // History and suggestions render asynchronously on slower machines.
                fun shown(matcher: SemanticsMatcher) =
                    compose.waitUntil(15000) {
                        runCatching { compose.onNode(matcher).assertIsDisplayed() }.isSuccess
                    }
                fun gone(matcher: SemanticsMatcher) =
                    compose.waitUntil(15000) {
                        compose.onAllNodes(matcher).fetchSemanticsNodes().isEmpty()
                    }
                shown(hasText("Merci d’utiliser \$review ici"))
                // The placeholder and history can render while connect() is still refreshing
                // the installation. A disabled TextField deliberately has no SetText action.
                compose.waitUntil(15000) {
                    compose
                        .onAllNodes(hasSetTextAction() and isEnabled())
                        .fetchSemanticsNodes()
                        .size == 1
                }
                val field = compose.onNode(hasSetTextAction())
                if (noSkills) {
                    val empty = hasText("Aucun skill disponible pour cette conversation")
                    field.performTextInput("$")
                    shown(empty)
                    shown(
                        hasText(
                            "Gérez les skills globaux et de projet dans la bibliothèque Skills."
                        )
                    )
                    compose.runOnIdle { checkNotNull(backDispatcher).onBackPressed() }
                    gone(hasTestTag("skill-suggestions"))
                    assertEquals(
                        "$",
                        field.fetchSemanticsNode().config[SemanticsProperties.EditableText].text,
                    )
                    field.performTextReplacement("")
                    field.performTextInput("\$css")
                    shown(empty)
                    field.performTextReplacement("\$5")
                    gone(hasTestTag("skill-suggestions"))
                    field.performTextReplacement("\$HOME")
                    gone(hasTestTag("skill-suggestions"))
                    field.performTextReplacement("\$css")
                    shown(empty)
                    compose.waitUntil(15000) {
                        compose
                            .onAllNodes(hasTestTag("conversation-send") and isEnabled())
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    compose.onNodeWithTag("conversation-send").performClick()
                    compose.waitUntil(10000) { sent.size == 1 }
                    assertEquals("\$css", sent[0]["text"]?.jsonPrimitive?.content)
                    return
                }
                field.performTextInput("\$unknown")
                shown(hasText("Aucun skill ne correspond à votre recherche"))
                field.performTextReplacement("")
                field.performTextInput("Lance $")
                shown(hasTestTag("skill-suggestion-review"))
                compose.onNodeWithTag("skill-suggestions").assertIsDisplayed()
                compose.onNodeWithTag("skill-suggestion-review").assertIsDisplayed()
                compose.onNodeWithTag("skill-suggestion-deploy").assertIsDisplayed()
                assertTrue(
                    compose
                        .onNodeWithTag("skill-suggestion-review")
                        .fetchSemanticsNode()
                        .boundsInRoot
                        .top <
                        compose
                            .onNodeWithTag("skill-suggestion-deploy")
                            .fetchSemanticsNode()
                            .boundsInRoot
                            .top
                )
                compose.onNodeWithTag("skill-suggestion-broken").assertDoesNotExist()
                field.performTextInput("re")
                gone(hasTestTag("skill-suggestion-deploy"))
                shown(hasText("Relire les changements en cours"))
                compose.onNodeWithTag("skill-suggestion-review").performClick()
                gone(hasTestTag("skill-suggestions"))
                assertEquals(
                    "Lance \$review ",
                    field.fetchSemanticsNode().config[SemanticsProperties.EditableText].text,
                )
                field.performTextInput("maintenant")
                // `$5` is not a skill and must not reopen the list.
                field.performTextInput(" pour \$5")
                compose.onNodeWithTag("skill-suggestions").assertDoesNotExist()
                compose.waitUntil(15000) {
                    compose
                        .onAllNodes(hasTestTag("conversation-send") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithTag("conversation-send").performClick()
                compose.waitUntil(10000) { sent.size == 1 }
                assertEquals(
                    "Lance \$review maintenant pour \$5",
                    sent[0]["text"]?.jsonPrimitive?.content,
                )
            } finally {
                // Dispose UI-owned connections before clearing the model and closing HTTP.
                compose.runOnIdle { mounted = false }
                compose.waitForIdle()
                compose.runOnIdle { models.clear() }
            }
        }
    }
}

private class SkillVault : SessionVault {
    private val values = mutableMapOf<String, String>()

    override fun read(origin: String) = values[origin]

    override fun write(origin: String, cookie: String?) {
        if (cookie == null) values.remove(origin) else values[origin] = cookie
    }
}
