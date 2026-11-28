package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.test.*
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import build.cairn.app.data.*
import build.cairn.contracts.RuleCatalogAgent
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class RulesCreationDeviceTest : RulesCreationCases() {
    @Test
    fun quickScheduleCreatesAnEditableClockRuleAndRunsItNow() {
        val requestBody = AtomicReference<JsonObject>()
        val previewBody = AtomicReference<JsonObject>()
        val ran = AtomicBoolean(false)
        val opened = AtomicReference<String>()
        val id = "00000000-0000-4000-8000-000000000020"
        val chatId = "00000000-0000-4000-8000-000000000021"
        val triggering =
            """
            {"id":"$id","ruleId":"$id","version":1,"status":"completed","createdAt":1,
             "updatedAt":1,"actions":1,"detailsVisible":true,
             "steps":[{"nodeId":"start_agent","label":"","kind":"startAgent","status":"completed",
             "result":{"conversationId":"$chatId"},"attempts":1,"startedAt":1}]}
        """
                .trimIndent()
        val catalog =
            rootSchemaCatalog()
                .copy(
                    agents =
                        listOf(
                            RuleCatalogAgent(
                                agentId = MAIN_AGENT_ID,
                                name = "Agent principal",
                                native = false,
                            )
                        )
                )
        fun reply(body: String) =
            MockResponse().setHeader("Content-Type", "application/json").setBody(body)
        fun saved(input: JsonObject) =
            """
            {"id":"$id","creatorAccountId":"fixture-account","definition":${input.getValue("definition")},
             "source":"app","revision":"revision","version":1,"createdAt":0,"updatedAt":0,
             "nativeAuthority":false,"allowed":{"runNow":true,"edit":true,"enable":false,
             "disable":true,"duplicate":true,"delete":true,"emergencyPause":true,
             "liftBudget":false,"viewDetails":true},"blocks":[],"warnings":[]}
        """
                .trimIndent()
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.requestUrl!!.encodedPath
                        return when {
                            path.endsWith("/hook-rules/catalog") ->
                                reply(wireJson.encodeToString(catalog))
                            path.endsWith("/hook-rules/schedule/preview") -> {
                                previewBody.set(
                                    wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                                )
                                reply(
                                    """{"timezone":"Europe/Paris","occurrences":[1796000000000]}"""
                                )
                            }
                            path.endsWith("/hook-rules/$id/run") -> {
                                ran.set(true)
                                reply(triggering)
                            }
                            path.endsWith("/hook-rules/$id/triggerings") ->
                                reply("""{"triggerings":[$triggering],"withoutEffect":0}""")
                            path.endsWith("/hook-rule-triggerings/$id") -> reply(triggering)
                            path.endsWith("/hook-rules/$id") && requestBody.get() != null ->
                                reply(saved(requestBody.get()))
                            path.endsWith("/hook-rules") && request.method == "POST" -> {
                                val input =
                                    wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                                requestBody.set(input)
                                reply(saved(input))
                            }
                            path.endsWith("/hook-rules") -> reply("""{"rules":[]}""")
                            path == "/api/account/session" -> reply(beaconAccountFixture())
                            path == "/api/installations" -> reply(beaconInstallationsFixture())
                            path.endsWith("/api/projects") -> reply("""{"projects":[]}""")
                            path.endsWith("/api/agents") -> reply("""{"agents":[]}""")
                            path.endsWith("/api/mcps") -> reply("""{"mcps":[]}""")
                            else -> reply("[]")
                        }
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
            val ui = FixtureUi(compose)
            try {
                ui.setContent(vm) {
                    LaunchedEffect(Unit) {
                        vm.state.first { it.ready }
                        vm.connect(origin)
                    }
                    val state by vm.state.collectAsState()
                    if (state.installation != null)
                        CairnTheme {
                            RulesScreen(vm, state, openConversation = { opened.set(it) }, edit = {})
                        }
                }
                compose.waitUntil(15000) {
                    compose
                        .onAllNodes(hasText("Programmer un agent") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Programmer un agent").performClick()
                compose.onNodeWithText("Nom").performTextInput("Revue du matin")
                compose.onNodeWithText("Brief").performTextInput("Examiner les changements.")
                compose
                    .onNodeWithText("Prévisualiser les prochaines dates")
                    .performScrollTo()
                    .performClick()
                compose.waitUntil(10000) { previewBody.get() != null }
                assertEquals("0 9 * * *", previewBody.get().text("cron"))
                compose.onNodeWithText("Créer la programmation").performClick()
                compose.waitUntil(15000) {
                    compose
                        .onAllNodesWithText("Lancer maintenant")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                val definition = requestBody.get().obj("definition")!!
                assertEquals("clock", definition.obj("trigger").text("type"))
                assertEquals("0 9 * * *", definition.obj("trigger").text("cron"))
                assertEquals(
                    "ruleConversationWorking",
                    definition
                        .list("steps")
                        .single()
                        .obj("condition")
                        .obj("condition")
                        .text("type"),
                )
                compose.onNodeWithText("Lancer maintenant").performClick()
                compose.waitUntil(10000) { ran.get() }
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Terminé · 1 actions")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Terminé · 1 actions").performClick()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Ouvrir la conversation")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Ouvrir la conversation").performClick()
                assertEquals(chatId, opened.get())
            } finally {
                ui.close()
            }
        }
    }
}
