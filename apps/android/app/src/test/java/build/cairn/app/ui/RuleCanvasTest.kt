package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import build.cairn.app.data.wireJson
import build.cairn.contracts.HookRuleCatalog
import java.io.File
import kotlinx.serialization.json.*
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-xhdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class RuleCanvasTest {
    @get:Rule val compose = createComposeRule()

    private val catalog: HookRuleCatalog =
        wireJson.decodeFromString(
            """{"agents":[{"agentId":"11111111-1111-4111-8111-111111111111","name":"Agent support","native":false},{"agentId":"22222222-2222-4222-8222-222222222222","name":"Relecteur","native":false}],
            "hooks":[
              {"agentIds":["11111111-1111-4111-8111-111111111111","22222222-2222-4222-8222-222222222222"],"title":"A message is posted","description":"In any channel or thread of the server.","pluginId":"discord","instanceName":"Acme","brand":{"color":"#5865F2","path":"M4 4h16v12H8l-4 4z"},"payloadSchema":null,"resourceFields":[],"trigger":{"type":"plugin","instanceId":"7d0b1e4f-2a3c-4d5e-8f90-112233445566","hook":"message"}},
              {"agentIds":["22222222-2222-4222-8222-222222222222"],"title":"A pull request is opened","description":"In a watched repository.","pluginId":"github","instanceName":"octo","payloadSchema":null,"resourceFields":[],"trigger":{"type":"plugin","instanceId":"8d0b1e4f-2a3c-4d5e-8f90-112233445566","hook":"pull-request-opened"}},
              {"agentIds":["11111111-1111-4111-8111-111111111111","22222222-2222-4222-8222-222222222222"],"title":"Reply ready","description":"An agent replied.","payloadSchema":null,"resourceFields":[],"trigger":{"type":"cairn","hook":"replyReady"}}
            ],
            "tools":[],"actions":[],"conditions":[],"operators":[],"functions":[],"variables":[],
            "limits":{"defaultLimits":{"triggeringsPerHour":60,"conversationsPerHour":10},"maxTriggeringsPerHour":600,"maxConversationsPerHour":100,"maxChainDepth":8,"maxConversationValuesBytes":262144,"maxWaitSeconds":604800,"maxSteps":500,"maxToolCalls":50,"maxNodes":200}}"""
        )

    private val steps =
        wireJson
            .parseToJsonElement(
                """[{"type":"if","id":"known","condition":{"type":"all","conditions":[]},"then":[{"type":"action","id":"start","action":{"type":"storeValue","scope":"rule","name":"last","value":{"type":"literal","value":1}}}],"otherwise":[]}]"""
            )
            .jsonArray

    @Composable
    private fun Canvas(chosen: Boolean, theme: String, attentionCount: Int = 0) {
        CairnTheme(theme) {
            Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                Column(Modifier.fillMaxSize()) {
                    RuleFlowTree(
                        if (chosen) steps else emptyList(),
                        "/steps",
                        catalog,
                        { "Agent support" },
                        editable = true,
                        fill = true,
                        attention = "trigger",
                        attentionCount = attentionCount,
                        modifier = Modifier.weight(1f),
                        start = {
                            RuleStartNode(
                                chosen = chosen,
                                hook = catalog.hooks.first().takeIf { chosen },
                                fallback = "message",
                                agentId = "11111111-1111-4111-8111-111111111111",
                                agentName = "Agent support",
                                problems = emptyList(),
                                attention = !chosen,
                                attentionCount = attentionCount,
                                enabled = true,
                                chooseEvent = {},
                                chooseAgent = {},
                            )
                        },
                    )
                }
            }
        }
    }

    @Test
    fun `a new rule starts from an empty node, then shows its event and agent`() {
        var chosen by mutableStateOf(false)
        compose.setContent { Canvas(chosen, "dark") }
        compose
            .onNodeWithContentDescription("Choisir ce qui déclenche la règle")
            .assertIsDisplayed()
        compose.onNodeWithText("Événement reçu").assertDoesNotExist()
        shot("rule-canvas-empty")
        chosen = true
        compose
            .onNodeWithContentDescription("Changer ce qui déclenche la règle : A message is posted")
            .assertIsDisplayed()
        compose.onNodeWithContentDescription("Avec les droits de Agent support").assertIsDisplayed()
        compose.onNodeWithText("Acme").assertIsDisplayed()
        shot("rule-canvas-graph")
    }

    @Test
    fun `the event picker groups events by instance and filters them`() {
        var picked = ""
        var query by mutableStateOf("")
        compose.setContent {
            CairnTheme("light") {
                Surface(Modifier.fillMaxSize()) {
                    RuleEventPicker(catalog, null, query, { query = it }, {}) {
                        picked = it.title
                    }
                }
            }
        }
        compose.onNodeWithText("Acme").assertIsDisplayed()
        compose.onNodeWithText("octo").assertIsDisplayed()
        compose.onNodeWithText("Conversations Cairn").assertIsDisplayed()
        query = "pull"
        compose.onNodeWithText("A message is posted").assertDoesNotExist()
        compose.onNodeWithText("A pull request is opened").performClick()
        assertEquals("A pull request is opened", picked)
    }

    @Test
    fun `only agents holding the hook can be chosen`() {
        var picked = ""
        compose.setContent {
            CairnTheme("light") {
                Surface(Modifier.fillMaxSize()) {
                    RuleAgentPicker(
                        catalog,
                        catalog.hooks[1],
                        "22222222-2222-4222-8222-222222222222",
                        { it },
                        {},
                    ) {
                        picked = it
                    }
                }
            }
        }
        compose.onNodeWithText("Ne peut pas écouter cet événement").assertIsDisplayed()
        compose.onNodeWithText("Agent support").performClick()
        assertEquals("", picked)
        compose.onNodeWithText("Relecteur").performClick()
        assertEquals("22222222-2222-4222-8222-222222222222", picked)
    }

    private fun shot(name: String) {
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
        File(dir).mkdirs()
        File(dir, "$name.png").outputStream().use {
            compose
                .onRoot()
                .captureToImage()
                .asAndroidBitmap()
                .compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
        }
    }
}
