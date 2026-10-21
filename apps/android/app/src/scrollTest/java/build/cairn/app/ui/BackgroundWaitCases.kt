package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/** The beacon session and relayed stream reach the same conversation UI on JVM and device. */
abstract class BackgroundWaitCases {
    @get:Rule val compose = createComposeRule()

    protected open fun capture(name: String) {}

    @Test
    fun recoveredQuotaDoesNotRestoreDuplicateErrors() =
        conversation("succeeded", quota = true) {
            compose.onNodeWithTag("chat-quota").assertDoesNotExist()
            compose.onNodeWithText(SPEND_LIMIT).assertDoesNotExist()
            compose.onNodeWithText("L’agent a rencontré une erreur").assertDoesNotExist()
            compose.onNodeWithText("Exécution en échec").assertDoesNotExist()
        }

    @Test
    fun quotaWaitFoldsDuplicateErrorsAndExpandsOneTechnicalMessage() =
        conversation("running", quota = true) {
            compose.onNodeWithTag("chat-quota").assertIsDisplayed()
            compose.onAllNodesWithText("Quota Claude Code atteint").assertCountEquals(1)
            compose
                .onNodeWithText("Reprise automatique dès qu’un compte est disponible.")
                .assertIsDisplayed()
            compose.onNodeWithText("En attente de quota").assertExists()
            compose.onAllNodesWithTag("agent-working").assertCountEquals(0)
            compose.onNodeWithText("L’agent a rencontré une erreur").assertDoesNotExist()
            compose.onNodeWithText("Exécution en échec").assertDoesNotExist()
            compose.onNodeWithText(SPEND_LIMIT).assertDoesNotExist()
            capture("quota-android-dark")
            compose.onNodeWithText("Détails techniques").performClick()
            compose.onAllNodesWithText(SPEND_LIMIT).assertCountEquals(1)
            compose.onNodeWithText(SPEND_LIMIT).performScrollTo().assertIsDisplayed()
            capture("quota-android-details")
        }

    @Test
    fun savedQuotaFailureOffersManualResumeInsteadOfPromisingAutomaticRecovery() =
        conversation("failed", quota = true) { server ->
            compose.onNodeWithTag("chat-quota").assertIsDisplayed()
            compose.onNodeWithText("Reprendre la conversation").assertExists()
            compose
                .onNodeWithText("Reprise automatique dès qu’un compte est disponible.")
                .assertDoesNotExist()
            compose.onNodeWithContentDescription("Masquer l’erreur").assertDoesNotExist()
            compose.onNodeWithText("L’agent a rencontré une erreur").assertDoesNotExist()
            compose.onNodeWithText("Exécution en échec").assertDoesNotExist()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Reprendre la conversation") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose
                .onNodeWithText("Reprendre la conversation")
                .performScrollTo()
                .assertIsDisplayed()
                .performClick()
            val effects = mutableListOf<String>()
            compose.waitUntil(10000) {
                compose.waitForIdle()
                var request = server.takeRequest(0, TimeUnit.MILLISECONDS)
                while (request != null) {
                    val path = request.path!!.substringBefore('?')
                    if (
                        request.method == "POST" &&
                            path in
                                listOf(
                                    "/api/installations/fixture/api/runs/run/resume",
                                    "/api/installations/fixture/api/chats/chat/pause",
                                )
                    )
                        effects.add(path)
                    request = server.takeRequest(0, TimeUnit.MILLISECONDS)
                }
                effects.size == 2
            }
            org.junit.Assert.assertEquals(
                listOf(
                    "/api/installations/fixture/api/runs/run/resume",
                    "/api/installations/fixture/api/chats/chat/pause",
                ),
                effects,
            )
        }

    @Test
    fun relayedBackgroundWaitReplacesWorking() =
        conversation("running") {
            compose.onNodeWithTag("agent-waiting").assertExists()
            compose.onAllNodesWithTag("agent-working").assertCountEquals(0)
            compose
                .onNodeWithTag("agent-waiting")
                .assert(
                    hasContentDescription(
                        "En attente d’une tâche en arrière-plan : Vérifier la CI. L’agent reprend quand elle se termine."
                    )
                )
            compose.onNodeWithText("Tâche en arrière-plan", substring = true).assertExists()
        }

    @Test
    fun headerWaitTimeAdvancesWithoutStreamEvents() {
        compose.setContent {
            CairnTheme {
                val started = remember { System.currentTimeMillis() - 58_000 }
                ConversationHeader(
                    title = "Conversation de test",
                    agent = "Agent principal",
                    agentKey = MAIN_AGENT_ID,
                    status = "",
                    live = "Tâche en arrière-plan",
                    liveSince = started,
                    route = ConnectionRoute.DIRECT,
                    back = {},
                    choose = {},
                    actions = {},
                )
            }
        }
        compose
            .onNodeWithText("Tâche en arrière-plan · < 1 min", useUnmergedTree = true)
            .assertExists()
        compose.waitUntil(5000) {
            compose
                .onAllNodesWithText("Tâche en arrière-plan · 1 min", useUnmergedTree = true)
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
    }

    @Test
    fun failedResponseBannerCanBeDismissed() =
        conversation("failed") {
            val message =
                "La réponse s’est arrêtée avant la fin. Reprenez la conversation pour continuer."
            compose.onNodeWithText(message).assertIsDisplayed()
            compose.onNodeWithText("Échec · Agent principal · Installation de test").assertExists()
            compose.onAllNodesWithTag("agent-waiting").assertCountEquals(0)
            compose.onNodeWithContentDescription("Masquer l’erreur").performClick()
            compose.onNodeWithText(message).assertDoesNotExist()
        }

    @Test
    fun scheduledSleepDisplaysAutomaticResumeWithoutWorkingIndicator() =
        conversation("succeeded", sleeping = true) {
            compose
                .onNodeWithText("Reprise dans 7 min", substring = true, useUnmergedTree = true)
                .assertExists()
            // The scheduling is outside the loaded history: the pending wait still has its card.
            compose.onNodeWithTag("chat-wait").assertIsDisplayed()
            compose.onNodeWithText("Reprise à ", substring = true).assertIsDisplayed()
            compose.onNodeWithText("dans 7 min").assertIsDisplayed()
            compose.onNodeWithText("Mettre en pause").assertIsDisplayed()
            compose.onNodeWithText("Répondre maintenant annule l’attente").assertExists()
            compose.onAllNodesWithTag("agent-working").assertCountEquals(0)
            compose.onAllNodesWithTag("agent-waiting").assertCountEquals(0)
            compose.onNodeWithContentDescription("Arrêter").assertExists()
        }

    @Test
    fun conditionWaitShowsResultAndDeadlineWithoutWorkingIndicator() =
        conversation("succeeded", conditional = true) {
            compose
                .onNodeWithText(
                    "Résultat attendu · 7 min max",
                    substring = true,
                    useUnmergedTree = true,
                )
                .assertExists()
            compose.onNodeWithTag("chat-wait").assertIsDisplayed()
            compose.onNodeWithText("Attente de la fin d’une autre exécution").assertIsDisplayed()
            compose.onNodeWithText("7 min max").assertIsDisplayed()
            compose.onAllNodesWithTag("agent-working").assertCountEquals(0)
            compose.onAllNodesWithTag("agent-waiting").assertCountEquals(0)
            compose.onNodeWithContentDescription("Arrêter").assertExists()
        }

    @Test
    fun scheduledWaitFollowsTheTurnAndClampsItsReason() {
        val reason =
            List(12) {
                    "Vérifier Quality et Android sur la PR, corriger les échecs puis poursuivre."
                }
                .joinToString(" ")
        conversation(
            "succeeded",
            conditional = true,
            reason = reason,
            history = { wakeup, now ->
                fun message(id: Long, text: String) =
                    RunEvent(
                        id,
                        now - 20_000 + id,
                        "item.completed",
                        "",
                        mapOf(
                            "item" to
                                buildJsonObject {
                                    put("id", "message-$id")
                                    put("type", "agent_message")
                                    put("text", text)
                                }
                        ),
                    )
                listOf(
                    RunEvent(1, now - 30_000, "turn.started", "", emptyMap()),
                    message(2, "Lot poussé dans la PR."),
                    RunEvent(
                        3,
                        now - 15_000,
                        "chat.wait",
                        reason,
                        wireJson.encodeToJsonElement(wakeup!!).jsonObject,
                    ),
                    message(4, "CI en cours, reprise programmée pour la vérifier."),
                    RunEvent(5, now - 5_000, "turn.completed", "", emptyMap()),
                )
            },
        ) {
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Agent principal").fetchSemanticsNodes().size == 2
            }
            // Messages render as rich text: each reply's author line marks where it starts.
            val replies =
                compose.onAllNodesWithText("Agent principal").fetchSemanticsNodes().map {
                    it.boundsInRoot
                }
            val card = compose.onNodeWithTag("chat-wait").fetchSemanticsNode().boundsInRoot
            // The wait starts once the turn ends: its card follows the agent's last reply.
            assertTrue(replies.all { card.top > it.bottom })
            compose.onNodeWithText("Voir plus").assertExists()
        }
    }

    @Test
    fun longWaitReasonIsClampedUntilExpanded() {
        val reason = List(40) { "Vérifier la CI puis poursuivre le lot suivant." }.joinToString(" ")
        val wakeup = ChatWakeup("wait", "message", reason, 60, 1, 60001)
        compose.setContent {
            CairnTheme {
                CompositionLocalProvider(LocalLiveWait provides LiveWait(wakeup, paused = false)) {
                    WaitCard(ActivityWait(wakeup))
                }
            }
        }
        compose.onNodeWithText("Voir plus").assertExists().performClick()
        compose.onNodeWithText("Voir moins").assertExists().performClick()
        compose.onNodeWithText("Voir plus").assertExists()
    }

    @Test
    fun resolvedWaitHonorsPauseAndCancellationUntilItResumes() {
        val wakeup =
            ChatWakeup(
                "wait",
                "message",
                "Attendre les checks",
                60,
                1,
                60001,
                condition =
                    buildJsonObject {
                        put("type", "github_pr_checks")
                        put("prNumber", 1)
                    },
            )
        val observation = buildJsonObject {
            put("state", "completed")
            put("type", "github_pr_checks")
            put("conclusion", "success")
        }
        val resolved = ActivityWait(wakeup, observation, observed = true)
        var wait by mutableStateOf(resolved)
        var live by mutableStateOf(LiveWait(wakeup, paused = false))
        compose.setContent {
            CairnTheme { CompositionLocalProvider(LocalLiveWait provides live) { WaitCard(wait) } }
        }
        compose
            .onNodeWithText("Reprise selon la capacité disponible", substring = true)
            .assertExists()
        compose.runOnIdle { live = live.copy(paused = true) }
        compose
            .onNodeWithText("Reprise quand vous reprendrez la conversation", substring = true)
            .assertExists()
        compose
            .onNodeWithText("Reprise selon la capacité disponible", substring = true)
            .assertDoesNotExist()
        compose.runOnIdle { live = LiveWait(null, paused = false) }
        compose.onNodeWithText("Attente annulée", substring = true).assertExists()
        compose.runOnIdle { wait = resolved.copy(ended = "cancelled") }
        compose.onNodeWithText("Attente annulée", substring = true).assertExists()
        compose.runOnIdle { wait = resolved.copy(ended = "replaced") }
        compose
            .onNodeWithText("Remplacée par un nouvel appel d’attente", substring = true)
            .assertExists()
        compose.runOnIdle { wait = resolved.copy(resumedAt = 1000) }
        compose.onNodeWithText("Checks requis réussis").assertExists()
        compose.runOnIdle { wait = ActivityWait(null, observation, observed = true) }
        compose.onNodeWithText("Le résultat était déjà disponible", substring = true).assertExists()
    }

    @Test
    fun explicitImmediateResultReplacesPendingWaitWithChangedWording() {
        val wakeup =
            ChatWakeup(
                "wake",
                "message",
                "Attente initiale",
                60,
                0,
                60000,
                condition =
                    buildJsonObject {
                        put("type", "run_finished")
                        put("runId", "dependency")
                    },
            )
        val observation = buildJsonObject {
            put("immediate", true)
            put("reason", "Revue terminée")
            put("state", "completed")
            put("type", "run_finished")
            put("runId", "dependency")
            put("status", "succeeded")
        }
        val waits =
            timelineEntries(
                    listOf(
                        RunEvent(
                            1,
                            1,
                            "chat.wait",
                            "",
                            wireJson.encodeToJsonElement(wakeup).jsonObject,
                        ),
                        RunEvent(2, 2, "chat.wait_ready", "Result available", observation),
                    ),
                    chat = true,
                )
                .mapNotNull { it.wait }
        compose.setContent {
            CairnTheme {
                androidx.compose.foundation.layout.Column { waits.forEach { WaitCard(it) } }
            }
        }
        compose.onAllNodesWithTag("chat-wait").assertCountEquals(2)
        compose
            .onNodeWithText("Remplacée par un nouvel appel d’attente", substring = true)
            .assertExists()
        compose.onNodeWithText("Le résultat était déjà disponible", substring = true).assertExists()
        compose.onNodeWithText("Revue terminée", substring = true).assertExists()
        compose.onNodeWithText("L’exécution est terminée").assertExists()
    }

    private fun conversation(
        status: String,
        sleeping: Boolean = false,
        conditional: Boolean = false,
        quota: Boolean = false,
        reason: String = "Attendre la CI",
        history: ((ChatWakeup?, Long) -> List<RunEvent>)? = null,
        check: (MockWebServer) -> Unit,
    ) {
        MockWebServer().use { server ->
            val now = System.currentTimeMillis()
            val chat =
                Chat(
                    "chat",
                    title = "Conversation de test",
                    agentId = MAIN_AGENT_ID,
                    agentName = "Agent principal",
                    runId = "run",
                    run =
                        Run(
                            "run",
                            status = status,
                            startedAt = now - 60_000,
                            snapshot =
                                Snapshot(
                                    agent = Agent(provider = if (quota) "claude" else "codex")
                                ),
                            accountWaitReason =
                                if (quota && status == "running")
                                    "Usage exhausted. Waiting for an available Claude Code account to resume."
                                else null,
                            error = if (quota && status == "failed") SPEND_LIMIT else null,
                        ),
                    wakeup =
                        if (sleeping || conditional)
                            ChatWakeup(
                                "wake",
                                "message",
                                reason,
                                420,
                                now,
                                now + 420000,
                                condition =
                                    if (conditional)
                                        buildJsonObject {
                                            put("type", "run_finished")
                                            put("runId", "other-run")
                                        }
                                    else null,
                            )
                        else null,
                )
            val waiting =
                RunEvent(
                    1,
                    now - 30_000,
                    "turn.waiting",
                    "",
                    mapOf(
                        "tasks" to
                            buildJsonArray {
                                addJsonObject { put("description", "Vérifier la CI") }
                            }
                    ),
                )
            val batch =
                wireJson.encodeToString(
                    LiveBatch(
                        if (quota) quotaEvents(now)
                        else history?.invoke(chat.wakeup, now) ?: listOf(waiting),
                        LiveState(chat = chat),
                        true,
                        false,
                    )
                )
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (path == "/api/installations/fixture/api/chats/chat/stream")
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .apply {
                                    val initial = "event: batch\nid: 1\ndata: $batch\n\n"
                                    if (quota) {
                                        // Keep the real SSE reader connected while inspecting the
                                        // card.
                                        setBody(initial + ": keepalive\n".repeat(100_000))
                                        throttleBody(
                                            initial.toByteArray().size.toLong(),
                                            250,
                                            TimeUnit.MILLISECONDS,
                                        )
                                    } else setBody(initial)
                                }

                        val body =
                            when (path) {
                                "/api/account/session" -> beaconAccountFixture()
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(
                                        listOf(Agent(MAIN_AGENT_ID, "Agent principal"))
                                    )
                                "/api/installations/fixture/api/chats/chat" ->
                                    wireJson.encodeToString(chat)
                                "/api/installations/fixture/api/overview" -> "{}"
                                else -> "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            server.start()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    BackgroundWaitVault(),
                    beaconOrigin = "",
                )
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready && !it.busy }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme(preference = if (quota) "dark" else "system") {
                    if (state.session.authenticated)
                        Surface(
                            Modifier.fillMaxSize(),
                            color = MaterialTheme.colorScheme.background,
                        ) {
                            Box(Modifier.safeDrawingPadding()) {
                                ChatScreen(
                                    vm,
                                    state,
                                    "chat",
                                    openChat = {},
                                    openRun = {},
                                    back = {},
                                )
                            }
                        }
                }
            }
            compose.waitUntil(20000) {
                compose
                    .onAllNodesWithText("Conversation de test")
                    .fetchSemanticsNodes()
                    .isNotEmpty() &&
                    (status != "running" ||
                        quota ||
                        compose
                            .onAllNodesWithTag("agent-waiting")
                            .fetchSemanticsNodes()
                            .isNotEmpty())
            }
            try {
                check(server)
            } finally {
                vm.api.closeStreams()
            }
        }
    }
}

private const val SPEND_LIMIT =
    "You've hit your individual spend limit · run /usage-credits to ask your admin for a higher limit · your weekly limit resets 3pm (UTC)"

private fun quotaEvents(now: Long): List<RunEvent> {
    fun event(
        id: Long,
        type: String,
        text: String = "",
        payload: Map<String, JsonElement>? = null,
    ) = RunEvent(id, now - 30_000 + id * 1000, type, text, payload, runId = "run")
    fun message(id: Long, text: String) =
        event(
            id,
            "item.completed",
            payload =
                mapOf(
                    "item" to
                        buildJsonObject {
                            put("id", "message-$id")
                            put("type", "agent_message")
                            put("text", text)
                        }
                ),
        )
    return listOf(
        event(
            1,
            "chat.user",
            "Corrige le dialogue de révocation.",
            mapOf("text" to JsonPrimitive("Corrige le dialogue de révocation.")),
        ),
        event(2, "turn.started"),
        message(
            3,
            "Le paramètre details est placé avant action. Les modifications sont conservées ; je poursuis les vérifications.",
        ),
        message(4, SPEND_LIMIT),
        event(
            5,
            "turn.failed",
            payload = mapOf("error" to buildJsonObject { put("message", SPEND_LIMIT) }),
        ),
        event(6, "status", "failed"),
    )
}

private class BackgroundWaitVault : SessionVault {
    override fun read(origin: String): String? = null

    override fun write(origin: String, cookie: String?) = Unit
}
