package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

abstract class RulesCreationCases {
    @get:Rule val compose = createComposeRule()

    @Test fun aLostCreationResponseKeepsTheLaterDraft() = recoverCreation(false)

    @Test fun aRecoveredCreationKeepsItsIdentityAcrossRecreation() = recoverCreation(true)

    @Test fun inversionsCannotPushAFullSubtreePastSixLevels() = conditionDepthControls(false)

    @Test
    fun aNegatedGroupCountsBothLevelsBeforeOfferingAnotherGroup() = conditionDepthControls(true)

    private fun conditionDepthControls(negated: Boolean) {
        var inner =
            ruleObject("type" to JsonPrimitive("formula"), "expression" to JsonPrimitive("true"))
        repeat(if (negated) 3 else 4) {
            inner =
                ruleObject("type" to JsonPrimitive("all"), "conditions" to JsonArray(listOf(inner)))
        }
        var value by
            mutableStateOf(
                ruleObject(
                    "type" to JsonPrimitive("all"),
                    "conditions" to
                        JsonArray(
                            listOf(
                                if (negated)
                                    ruleObject("type" to JsonPrimitive("not"), "condition" to inner)
                                else inner
                            )
                        ),
                )
            )
        assertEquals(6, ruleConditionDepth(value))
        val vm =
            CairnViewModel(
                ApplicationProvider.getApplicationContext<Application>(),
                object : SessionVault {
                    override fun read(origin: String): String? = null

                    override fun write(origin: String, cookie: String?) {}
                },
                beaconOrigin = "",
            )
        val models = ViewModelStore().apply { put("fixture", vm) }
        try {
            compose.setContent {
                CairnTheme {
                    Column(Modifier.verticalScroll(rememberScrollState())) {
                        RuleConditionEditor(value, emptyList(), { value = it }, vm, null)
                    }
                }
            }
            val inversions = compose.onAllNodesWithText("Inverser")
            assertTrue(inversions.fetchSemanticsNodes().isNotEmpty())
            for (i in inversions.fetchSemanticsNodes().indices) inversions[i].assertIsNotEnabled()
            if (negated) {
                compose
                    .onAllNodesWithText("+ Ajouter une condition")[0]
                    .performScrollTo()
                    .performClick()
                compose.onNodeWithText("Groupe ET").assertDoesNotExist()
                compose.onNodeWithText("Groupe OU").assertDoesNotExist()
            }
        } finally {
            compose.runOnIdle { models.clear() }
        }
    }

    @Test fun aKnownCreationRejectionAllowsCorrectingTheDraft() = recoverCreation(false, true)

    private fun recoverCreation(restore: Boolean, reject: Boolean = false) {
        val id = "da2dd0ad-91ce-4566-b426-77c03628f59e"
        val unavailable = AtomicBoolean(true)
        val failureMessage = if (reject) "Creation rejected" else "Uncertain creation"
        val closed = AtomicBoolean(false)
        val stored = AtomicReference<JsonObject?>(null)
        val writes = CopyOnWriteArrayList<Pair<String, JsonObject>>()
        fun record(definition: JsonObject, version: Int) = buildJsonObject {
            put("id", id)
            put("revision", "revision-$version")
            put("version", version)
            put("definition", definition)
            put("allowed", buildJsonObject { put("edit", true) })
        }
        fun json(body: String, status: Int = 200) =
            MockResponse()
                .setResponseCode(status)
                .setHeader("Content-Type", "application/json")
                .setBody(body)
        val catalog =
            """{"agents":[{"agentId":"$MAIN_AGENT_ID","name":"Assistant","native":false}],"hooks":[{"agentIds":["$MAIN_AGENT_ID"],"title":"Réponse prête","description":"","payloadSchema":null,"resourceFields":[],"trigger":{"type":"cairn","hook":"replyReady"}}],"tools":[],"actions":[],"conditions":[],"operators":[],"functions":[],"variables":[],"limits":{"defaultLimits":{"triggeringsPerHour":60,"conversationsPerHour":5},"maxTriggeringsPerHour":100,"maxConversationsPerHour":10,"maxWaitSeconds":604800,"maxSteps":500,"maxToolCalls":100,"maxNodes":100}}"""
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.requestUrl!!.encodedPath
                        if (path.endsWith("/hook-rules/validate"))
                            return json("""{"valid":true,"problems":[],"warnings":[]}""")
                        if (path.endsWith("/hook-rules/catalog")) return json(catalog)
                        if (request.method == "POST" && path.endsWith("/hook-rules")) {
                            val body =
                                wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                            writes += "POST" to body
                            if (!unavailable.get() || !reject)
                                stored.compareAndSet(null, record(body.obj("definition")!!, 1))
                            return if (unavailable.get())
                                json("""{"error":"$failureMessage"}""", if (reject) 400 else 503)
                            else json(stored.get().toString())
                        }
                        if (path.endsWith("/hook-rules/$id")) {
                            if (request.method == "PUT") {
                                val body =
                                    wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                                writes += "PUT" to body
                                stored.set(record(body.obj("definition")!!, 2))
                            }
                            return json(stored.get().toString())
                        }
                        return json(
                            when {
                                path == "/api/account/session" -> beaconAccountFixture()
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path.endsWith("/api/projects") -> """{"projects":[]}"""
                                path.endsWith("/api/agents") -> """{"agents":[]}"""
                                path.endsWith("/api/tasks") -> """{"tasks":[]}"""
                                path.endsWith("/api/mcps") -> """{"mcps":[]}"""
                                else -> "[]"
                            }
                        )
                    }
                }
            val origin = server.url("/").toString()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    object : SessionVault {
                        override fun read(origin: String): String? = null

                        override fun write(origin: String, cookie: String?) {}
                    },
                    beaconOrigin = "",
                )
            val models = ViewModelStore().apply { put("fixture", vm) }
            val restoration = StateRestorationTester(compose)
            restoration.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(origin)
                }
                val state by vm.state.collectAsState()
                CairnTheme {
                    if (state.installation != null && !closed.get())
                        RuleEditorScreen(vm, state, "", saved = { closed.set(true) }, back = {})
                }
            }
            fun waitFor(test: () -> Boolean) =
                compose.waitUntil(20000) {
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    test()
                }
            try {
                waitFor {
                    compose.onAllNodesWithText("Nom de la règle").fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNodeWithText("Nom de la règle").performTextReplacement("Original")
                compose.onNodeWithText("Étape").performScrollTo().performClick()
                compose.onNodeWithText("Mémoriser une valeur").performClick()
                compose.onNodeWithText("Terminer cette étape").performScrollTo().performClick()
                waitFor {
                    compose
                        .onAllNodes(hasText("Créer") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Créer").performClick()
                waitFor {
                    compose
                        .onAllNodesWithText(failureMessage, substring = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Nom de la règle").performTextReplacement("Latest draft")
                unavailable.set(false)
                waitFor {
                    compose
                        .onAllNodes(hasText("Créer") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Créer").performClick()
                if (reject) {
                    waitFor { closed.get() }
                    assertTrue(writes.none { it.first == "PUT" })
                    assertEquals("Original", writes.first().second.obj("definition").text("name"))
                    assertEquals(
                        "Latest draft",
                        writes.last().second.obj("definition").text("name"),
                    )
                    assertEquals(1, writes.map { it.second.text("creationId") }.toSet().size)
                    assertEquals("Latest draft", stored.get().obj("definition").text("name"))
                    return
                }
                waitFor {
                    compose
                        .onAllNodesWithText(
                            "Vos dernières modifications sont conservées",
                            substring = true,
                        )
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                assertFalse(closed.get())
                compose.onNodeWithText("Nom de la règle").assertTextContains("Latest draft")
                if (restore) {
                    restoration.emulateSavedInstanceStateRestore()
                    waitFor {
                        compose
                            .onAllNodes(hasText("Enregistrer") and isEnabled())
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    compose.onNodeWithText("Enregistrer").performClick()
                } else {
                    compose.onNodeWithText("Enregistrer mon brouillon à la suite").performClick()
                }
                waitFor { closed.get() }
                assertEquals(
                    1,
                    writes
                        .filter { it.first == "POST" }
                        .map { it.second.text("creationId") }
                        .toSet()
                        .size,
                )
                assertTrue(
                    writes
                        .filter { it.first == "POST" }
                        .all { it.second.obj("definition").text("name") == "Original" }
                )
                val update = writes.single { it.first == "PUT" }.second
                assertEquals(id, update.text("ruleId"))
                assertEquals("Latest draft", update.obj("definition").text("name"))
                assertEquals(2, stored.get()!!["version"]!!.jsonPrimitive.int)
            } finally {
                compose.runOnIdle { models.clear() }
            }
        }
    }
}
