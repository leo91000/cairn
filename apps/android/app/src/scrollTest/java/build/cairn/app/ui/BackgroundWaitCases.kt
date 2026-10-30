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
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/** The beacon session and relayed stream reach the same conversation UI on JVM and device. */
abstract class BackgroundWaitCases {
    @get:Rule val compose = createComposeRule()

    protected open fun capture(name: String) {}

    @Test
    fun rejectedQuotaSwitchRemainsVisibleAfterOptionsRefresh() =
        conversation("running", quota = true, codingAgentChoices = true, rejectSwitch = true) {
            compose.waitUntil(5000) {
                compose
                    .onAllNodesWithText("Continuer avec Codex")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Continuer avec Codex").performScrollTo().performClick()
            // The changed label proves the options response after the POST was rendered.
            compose.waitUntil(5000) {
                compose
                    .onAllNodesWithText("Continuer avec Codex disponible")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Switch refused. Review the accounts.").assertExists()
        }

    @Test
    fun quotaAlternativesRefreshWhenCodexBecomesAvailable() =
        conversation(
            "running",
            quota = true,
            codingAgentChoices = true,
            initialAlternatives = "unavailable",
        ) {
            compose.waitUntil(15000) {
                compose
                    .onAllNodesWithText("Continuer avec Codex")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Continuer avec Codex").performScrollTo().assertIsDisplayed()
        }

    @Test
    fun quotaAlternativesRetryAfterAReadFailure() =
        conversation(
            "running",
            quota = true,
            codingAgentChoices = true,
            initialAlternatives = "error",
        ) {
            compose.waitUntil(15000) {
                compose
                    .onAllNodesWithText("Continuer avec Codex")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Continuer avec Codex").performScrollTo().assertIsDisplayed()
            compose.onNodeWithText("Alternatives temporarily unavailable.").assertDoesNotExist()
        }

    @Test
    fun quotaOffersOneClickCodingAgentSwitchForTheCurrentTurn() =
        conversation("running", quota = true, codingAgentChoices = true) { server, _ ->
            compose.waitUntil(5000) {
                compose
                    .onAllNodesWithText("Continuer avec Codex")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Continuer avec Codex").performScrollTo().assertIsDisplayed()
            capture("quota-switch-android")
            compose.onNodeWithText("Continuer avec Codex").performClick()
            val request = generateSequence {
                server.takeRequest(5, TimeUnit.SECONDS)
            }
                .first { it.method == "POST" && it.path?.endsWith("/coding-agent") == true }
            val body = Json.parseToJsonElement(request.body.readUtf8()).jsonObject
            assertTrue(body["provider"] == JsonPrimitive("codex"))
            assertTrue(body["runId"] == JsonPrimitive(SAVED_RUN_ID))
            assertTrue(body["turnId"] == JsonPrimitive("quota-turn"))
            assertTrue(body["requestId"]!!.jsonPrimitive.content.isNotBlank())
            compose.waitUntil(5000) {
                compose
                    .onAllNodes(hasText("Annuler le changement") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Annuler le changement").performScrollTo().performClick()
            compose.waitUntil(5000) {
                compose
                    .onAllNodes(hasText("Continuer avec Codex") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithTag("model-picker").performClick()
            compose.onNodeWithText("Claude Code").assertIsSelected()
            compose.onNodeWithText("Codex").assertIsNotSelected()
        }

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
            compose
                .onAllNodesWithText("Limite de dépense Claude Code atteinte")
                .assertCountEquals(1)
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
    fun laterAdmissionWaitDoesNotRestoreRecoveredQuotaDetails() =
        conversation(
            "queued",
            quota = true,
            history = { _, now ->
                quotaEvents(now) +
                    listOf(
                        RunEvent(
                            id = 7,
                            createdAt = now,
                            type = "turn.started",
                            text = "",
                            runId = SAVED_RUN_ID,
                        ),
                        RunEvent(
                            id = 8,
                            createdAt = now,
                            type = "turn.completed",
                            text = "",
                            runId = SAVED_RUN_ID,
                        ),
                        RunEvent(
                            id = 9,
                            createdAt = now,
                            type = "status",
                            text = "succeeded",
                            runId = SAVED_RUN_ID,
                        ),
                    )
            },
        ) {
            compose.onNodeWithTag("chat-quota").assertIsDisplayed()
            compose.onNodeWithText("Quota Claude Code atteint").assertIsDisplayed()
            compose
                .onNodeWithText("La limite de dépense du compte est atteinte.")
                .assertDoesNotExist()
            compose
                .onNodeWithText("Réinitialisation annoncée", substring = true)
                .assertDoesNotExist()
            compose.onNodeWithText("Détails techniques").assertDoesNotExist()
            compose.onNodeWithText(SPEND_LIMIT).assertDoesNotExist()
        }

    @Test
    fun quotaWaitRemainsAutomaticWhileFollowupQueueIsPaused() =
        conversation("running", quota = true, paused = true) {
            compose.onNodeWithTag("chat-quota").assertIsDisplayed()
            compose.onNodeWithText("En pause").assertExists()
            compose
                .onNodeWithText("Reprise automatique dès qu’un compte est disponible.")
                .assertIsDisplayed()
            compose.onNodeWithText("Reprendre la conversation").assertDoesNotExist()
            compose.onAllNodesWithTag("agent-working").assertCountEquals(0)
        }

    @Test
    fun aMemberWithoutAccessSeesWhyTheConversationStoppedWithoutItsActions() =
        conversation("failed", quota = true, readOnly = true) {
            compose.onNodeWithTag("chat-quota").assertIsDisplayed()
            compose.onNodeWithText("Limite de dépense Claude Code atteinte").assertExists()
            compose.onNodeWithText("Reprendre la conversation").assertDoesNotExist()
            compose
                .onNodeWithText(
                    "Demandez au propriétaire de l’installation de vérifier les comptes."
                )
                .assertDoesNotExist()
            compose.onNodeWithText("L’agent a rencontré une erreur").assertDoesNotExist()
        }

    @Test
    fun losingAgentAccessClosesAnOpenQuestionWithoutAnsweringOrWithdrawingIt() {
        val usableAgents = AtomicReference(listOf(MAIN_AGENT_ID))
        val question =
            questionFixture(
                "pending-question",
                "6cc6efab-10af-54c7-a91b-dcff664166a1",
                fields =
                    listOf(
                        fieldFixture(
                            "scope",
                            "Quelle portée ?",
                            options = listOf(optionFixture("Un seul module", id = "module")),
                        )
                    ),
            )

        conversation(
            "running",
            questions = listOf(question),
            usableAgents = { usableAgents.get() },
        ) { server, vm ->
            compose.onNodeWithText("1 question · Répondre").performClick()
            compose.onNodeWithText("Un seul module").performClick()
            compose.onNodeWithText("Envoyer la réponse").assertIsEnabled()
            compose.onNodeWithText("Ce n’est plus utile").assertExists()

            usableAgents.set(emptyList())
            compose.runOnIdle { vm.perform { refreshUsableAgents() } }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithTag("conversation-read-only")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Votre réponse").assertDoesNotExist()
            compose.onNodeWithText("Envoyer la réponse").assertDoesNotExist()
            compose.onNodeWithText("Ce n’est plus utile").assertDoesNotExist()
            compose.onNodeWithText("1 question · Répondre").assertDoesNotExist()
            compose
                .onNodeWithText("1 question attend une personne ayant accès à cet agent.")
                .assertExists()

            usableAgents.set(listOf(MAIN_AGENT_ID))
            compose.runOnIdle { vm.perform { refreshUsableAgents() } }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("1 question · Répondre")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Votre réponse").assertDoesNotExist()
            compose.onNodeWithText("Envoyer la réponse").assertDoesNotExist()
            compose.onNodeWithText("Ce n’est plus utile").assertDoesNotExist()

            val requests = List(server.requestCount) { server.takeRequest(5, TimeUnit.SECONDS)!! }
            assertEquals(
                0,
                requests.count {
                    it.method == "POST" &&
                        (it.path?.endsWith("/answer") == true ||
                            it.path?.endsWith("/cancel") == true)
                },
            )
        }
    }

    @Test
    fun savedQuotaFailureOffersManualResumeInsteadOfPromisingAutomaticRecovery() =
        conversation("failed", quota = true) { server, _ ->
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
                                    "/api/installations/fixture/api/runs/$SAVED_RUN_ID/resume",
                                    "/api/installations/fixture/api/chats/6cc6efab-10af-54c7-a91b-dcff664166a1/pause",
                                )
                    )
                        effects.add(path)
                    request = server.takeRequest(0, TimeUnit.MILLISECONDS)
                }
                effects.size == 2
            }
            org.junit.Assert.assertEquals(
                listOf(
                    "/api/installations/fixture/api/runs/$SAVED_RUN_ID/resume",
                    "/api/installations/fixture/api/chats/6cc6efab-10af-54c7-a91b-dcff664166a1/pause",
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
                        id = id,
                        createdAt = now - 20_000 + id,
                        type = "item.completed",
                        text = "",
                        payload =
                            kotlinx.serialization.json.JsonObject(
                                mapOf(
                                    "item" to
                                        buildJsonObject {
                                            put("id", "message-$id")
                                            put("type", "agent_message")
                                            put("text", text)
                                        }
                                )
                            ),
                    )
                listOf(
                    RunEvent(
                        id = 1,
                        createdAt = now - 30_000,
                        type = "turn.started",
                        text = "",
                        payload = kotlinx.serialization.json.JsonObject(emptyMap()),
                    ),
                    message(2, "Lot poussé dans la PR."),
                    RunEvent(
                        id = 3,
                        createdAt = now - 15_000,
                        type = "chat.wait",
                        text = reason,
                        payload =
                            kotlinx.serialization.json.JsonObject(
                                wireJson.encodeToJsonElement(wakeup!!).jsonObject
                            ),
                    ),
                    message(4, "CI en cours, reprise programmée pour la vérifier."),
                    RunEvent(
                        id = 5,
                        createdAt = now - 5_000,
                        type = "turn.completed",
                        text = "",
                        payload = kotlinx.serialization.json.JsonObject(emptyMap()),
                    ),
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
        val wakeup = wakeupFixture("wait", "message", reason, 60, 1, 60001)
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
            wakeupFixture(
                "wait",
                "message",
                "Attendre les checks",
                60,
                1,
                60001,
                condition =
                    buildJsonObject {
                        put("type", "github_pr_checks")
                        put("pluginId", "github")
                        put("prNumber", 1)
                    },
                display =
                    buildJsonObject {
                        put("title", "Waiting for required checks on PR #1")
                        put("short", "PR #1 checks")
                        putJsonArray("details") {}
                    },
            )
        // The plugin words its result; the app shows it as given.
        val observation = buildJsonObject {
            put("state", "completed")
            put("type", "github_pr_checks")
            put("pluginId", "github")
            put("conclusion", "success")
            putJsonObject("display") {
                put("tone", "ok")
                put("title", "Required checks passed")
                put("detail", "1 check")
            }
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
        compose.onNodeWithText("Required checks passed").assertExists()
        compose.onNodeWithText("PR #1 checks", substring = true).assertExists()
        compose.runOnIdle { wait = ActivityWait(null, observation, observed = true) }
        compose.onNodeWithText("Le résultat était déjà disponible", substring = true).assertExists()
    }

    @Test
    fun completedTimeAndExternalTimeoutHaveDifferentCards() {
        val reached = buildJsonObject {
            put("state", "completed")
            put("type", "time")
            put("wakeAt", 1000)
        }
        val timedOut = buildJsonObject { put("state", "timed_out") }
        compose.setContent {
            CairnTheme {
                androidx.compose.foundation.layout.Column {
                    WaitCard(ActivityWait(null, reached, observed = true))
                    WaitCard(ActivityWait(null, timedOut, observed = true))
                }
            }
        }
        compose.onNodeWithText("Échéance atteinte").assertExists()
        compose.onNodeWithText("Heure prévue", substring = true).assertExists()
        compose.onNodeWithText("Échéance atteinte sans résultat").assertExists()
    }

    @Test
    fun explicitImmediateResultReplacesPendingWaitWithChangedWording() {
        val wakeup =
            wakeupFixture(
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
                            id = 1,
                            createdAt = 1,
                            type = "chat.wait",
                            text = "",
                            payload =
                                kotlinx.serialization.json.JsonObject(
                                    wireJson.encodeToJsonElement(wakeup).jsonObject
                                ),
                        ),
                        RunEvent(
                            id = 2,
                            createdAt = 2,
                            type = "chat.wait_ready",
                            text = "Result available",
                            payload = kotlinx.serialization.json.JsonObject(observation),
                        ),
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
        codingAgentChoices: Boolean = false,
        initialAlternatives: String? = null,
        rejectSwitch: Boolean = false,
        paused: Boolean = false,
        // A member who cannot use the conversation's agent.
        readOnly: Boolean = false,
        questions: List<ChatQuestion> = emptyList(),
        usableAgents: (() -> List<String>)? = null,
        reason: String = "Attendre la CI",
        history: ((ChatWakeup?, Long) -> List<RunEvent>)? = null,
        check: (MockWebServer, CairnViewModel) -> Unit,
    ) {
        MockWebServer().use { server ->
            val now = System.currentTimeMillis()
            val chat =
                Chat(
                    id = "6cc6efab-10af-54c7-a91b-dcff664166a1",
                    title = "Conversation de test",
                    agentId = MAIN_AGENT_ID,
                    agentName = "Agent principal",
                    paused = paused,
                    questions = questions,
                    runId = SAVED_RUN_ID,
                    run =
                        Run(
                            id = SAVED_RUN_ID,
                            status = status,
                            startedAt = now - 60_000,
                            snapshot =
                                Snapshot(
                                    agent =
                                        Agent(
                                            exclusiveNodes = emptyList(),
                                            plugins = emptyList(),
                                            id = "00000000-0000-4000-8000-000000000000",
                                            name = "",
                                            revision = "",
                                            createdAt = 0,
                                            access =
                                                AccessPolicy(
                                                    mcpTools =
                                                        build.cairn.contracts.McpTools(emptyMap()),
                                                    projects = null,
                                                    skills = null,
                                                    mcps = null,
                                                ),
                                            provider = if (quota) "claude" else "codex",
                                        ),
                                    task =
                                        build.cairn.contracts.RunTaskSnapshot(
                                            agentId = build.cairn.app.data.MAIN_AGENT_ID,
                                            id = "00000000-0000-4000-8000-000000000003",
                                            name = "Fixture",
                                            prompt = "Fixture",
                                        ),
                                ),
                            accountWaitReason =
                                when {
                                    quota && status == "running" ->
                                        "Usage exhausted. Waiting for an available Claude Code account to resume."
                                    quota && status == "queued" ->
                                        "Waiting for a Claude Code account with available usage."
                                    else -> null
                                },
                            error = if (quota && status == "failed") SPEND_LIMIT else null,
                            taskId = "",
                        ),
                    wakeup =
                        if (sleeping || conditional)
                            wakeupFixture(
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
                    id = 1,
                    createdAt = now - 30_000,
                    type = "turn.waiting",
                    text = "",
                    payload =
                        kotlinx.serialization.json.JsonObject(
                            mapOf(
                                "tasks" to
                                    buildJsonArray {
                                        addJsonObject { put("description", "Vérifier la CI") }
                                    }
                            )
                        ),
                )
            val batch =
                wireJson.encodeToString(
                    LiveBatch(
                        history?.invoke(chat.wakeup, now)
                            ?: if (quota) quotaEvents(now) else listOf(waiting),
                        LiveState(chat = chat),
                        true,
                        false,
                    )
                )
            server.dispatcher =
                object : Dispatcher() {
                    @Volatile var switching = false
                    @Volatile var switchRejected = false
                    var alternativesReads = 0

                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (codingAgentChoices && path.endsWith("/stop")) {
                            switching = false
                            return MockResponse()
                                .setHeader("Content-Type", "application/json")
                                .setBody("{}")
                        }
                        if (codingAgentChoices && path.endsWith("/coding-agent")) {
                            if (request.method == "POST" && rejectSwitch) {
                                switchRejected = true
                                return MockResponse()
                                    .setResponseCode(409)
                                    .setHeader("Content-Type", "application/json")
                                    .setBody("""{"error":"Switch refused. Review the accounts."}""")
                            }
                            if (request.method == "GET" && alternativesReads++ == 0) {
                                if (initialAlternatives == "error")
                                    return MockResponse()
                                        .setResponseCode(500)
                                        .setHeader("Content-Type", "application/json")
                                        .setBody(
                                            """{"error":"Alternatives temporarily unavailable."}"""
                                        )
                                if (initialAlternatives == "unavailable")
                                    return MockResponse()
                                        .setHeader("Content-Type", "application/json")
                                        .setBody(
                                            """{"runId":"$SAVED_RUN_ID","turnId":"quota-turn","provider":"claude","choices":[]}"""
                                        )
                            }
                            if (request.method == "POST") switching = true
                            val progress =
                                if (switching) ""","switching":{"provider":"codex","error":null}"""
                                else ""
                            val label = if (switchRejected) "Codex disponible" else "Codex"
                            return MockResponse()
                                .setHeader("Content-Type", "application/json")
                                .setBody(
                                    if (request.method == "POST") "{}"
                                    else
                                        """{"runId":"$SAVED_RUN_ID","turnId":"quota-turn","provider":"claude","choices":[{"provider":"codex","label":"$label"}]$progress}"""
                                )
                        }

                        if (
                            path ==
                                "/api/installations/fixture/api/chats/6cc6efab-10af-54c7-a91b-dcff664166a1/stream"
                        )
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
                                "/api/account/session" ->
                                    beaconAccountFixture().let {
                                        if (usableAgents == null) it
                                        else it.replace("\"role\":\"owner\"", "\"role\":\"member\"")
                                    }
                                "/api/installations" ->
                                    beaconInstallationsFixture().let {
                                        if (usableAgents == null) it
                                        else it.replace("\"role\":\"owner\"", "\"role\":\"member\"")
                                    }
                                "/api/installations/fixture/api/usable-agents" ->
                                    wireJson.encodeToString(
                                        build.cairn.contracts.UsableAgentsOutput(
                                            usableAgents?.invoke().orEmpty()
                                        )
                                    )
                                "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(
                                        build.cairn.contracts.ListAgentsOutput(
                                            listOf(
                                                Agent(
                                                    exclusiveNodes = emptyList(),
                                                    plugins = emptyList(),
                                                    revision = "",
                                                    createdAt = 0,
                                                    access =
                                                        AccessPolicy(
                                                            mcpTools =
                                                                build.cairn.contracts.McpTools(
                                                                    emptyMap()
                                                                ),
                                                            projects = null,
                                                            skills = null,
                                                            mcps = null,
                                                        ),
                                                    id = MAIN_AGENT_ID,
                                                    name = "Agent principal",
                                                )
                                            )
                                        )
                                    )
                                "/api/installations/fixture/api/chats/6cc6efab-10af-54c7-a91b-dcff664166a1" ->
                                    wireJson.encodeToString(chat)
                                "/api/installations/fixture/api/overview" ->
                                    installationOverviewFixture()
                                "/api/installations/fixture/api/runs/$SAVED_RUN_ID/resume" ->
                                    wireJson.encodeToString(
                                        build.cairn.contracts.ResumeRunOutput(SAVED_RUN_ID)
                                    )
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
                                    if (readOnly) state.copy(usableAgents = emptyList()) else state,
                                    "6cc6efab-10af-54c7-a91b-dcff664166a1",
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
                check(server, vm)
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
    ) =
        RunEvent(
            id = id,
            createdAt = now - 30_000 + id * 1000,
            type = type,
            text = text,
            payload = (payload)?.let { kotlinx.serialization.json.JsonObject(it) },
            runId = SAVED_RUN_ID,
        )
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

private const val SAVED_RUN_ID = "123e4567-e89b-42d3-a456-426614174000"
