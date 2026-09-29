package dev.leo.manager.ui

import android.app.Application
import androidx.activity.OnBackPressedDispatcher
import androidx.activity.compose.LocalOnBackPressedDispatcherOwner
import androidx.compose.runtime.*
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import dev.leo.manager.data.*
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

    private fun skillJourney(noSkills: Boolean) {
        MockWebServer().use { server ->
            val sent = CopyOnWriteArrayList<JsonObject>()
            val agent = Agent(MAIN_AGENT_ID, "Agent principal")
            val run = Run("run", status = "succeeded", snapshot = Snapshot(agent = agent))
            val chat =
                Chat("chat", title = "Skills", runId = "run", agentName = agent.name, run = run)
            val skills =
                if (noSkills) emptyList()
                else
                    listOf(
                        Skill("review", "Relire les changements en cours"),
                        Skill("deploy", "Publier une version"),
                        Skill("broken", "Invalide", valid = false),
                    )
            val history =
                listOf(RunEvent(1, 1789315200000, "chat.user", "Merci d’utiliser \$review ici"))
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (path == "/api/chats/chat/stream") {
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
                                "/api/session" -> """{"authenticated":true,"csrf":"fixture"}"""
                                "/api/agents" -> wireJson.encodeToString(listOf(agent))
                                "/api/skills" -> wireJson.encodeToString(skills)
                                "/api/chats/chat/messages" -> {
                                    sent +=
                                        wireJson
                                            .parseToJsonElement(request.body.readUtf8())
                                            .jsonObject
                                    "{}"
                                }
                                "/api/codex/models" -> """{"models":[]}"""
                                "/api/overview" -> "{}"
                                else -> "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vm =
                LeoViewModel(ApplicationProvider.getApplicationContext<Application>(), SkillVault())
            var backDispatcher: OnBackPressedDispatcher? = null
            compose.setContent {
                backDispatcher = LocalOnBackPressedDispatcherOwner.current?.onBackPressedDispatcher
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    if (!vm.state.value.session.authenticated)
                        vm.connect(server.url("/").toString())
                }
                LeoTheme {
                    if (state.session.authenticated)
                        ChatScreen(vm, state, "chat", openChat = {}, openRun = {})
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
            val field = compose.onNode(hasSetTextAction())
            if (noSkills) {
                val empty = hasText("Aucun skill disponible pour cette conversation")
                field.performTextInput("$")
                shown(empty)
                shown(hasText("Gérez les skills globaux et de projet dans la bibliothèque Skills."))
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
