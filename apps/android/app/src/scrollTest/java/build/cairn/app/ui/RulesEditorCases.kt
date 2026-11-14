package build.cairn.app.ui

import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import build.cairn.app.data.*
import build.cairn.contracts.HookRuleProblem
import kotlinx.serialization.json.*
import org.junit.*
import org.junit.Assert.*

abstract class RulesEditorCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun nestedFlowInsertsIntoTheChosenBranch() {
        var definition by
            mutableStateOf<JsonElement>(
                ruleObject(
                    "executionAgentId" to JsonPrimitive(MAIN_AGENT_ID),
                    "steps" to JsonArray(emptyList()),
                )
            )
        compose.setContent {
            CairnTheme {
                RuleFlowTree(
                    definition.list("steps"),
                    "/steps",
                    null,
                    { "Assistant" },
                    editable = true,
                    insert = { path, at, kind ->
                        definition =
                            definition.inserted(path, at, newRuleStep(definition, kind, null))
                    },
                )
            }
        }
        compose.onNodeWithText("Étape").performClick()
        compose.onNodeWithText("Si / sinon").performClick()
        compose.onNodeWithText("ALORS").assertIsDisplayed()
        compose.onNodeWithText("SINON").assertIsDisplayed()
        compose.onAllNodesWithText("Étape")[1].performClick()
        compose.onNodeWithText("Mémoriser une valeur").performClick()
        compose.runOnIdle {
            val branch = definition.list("steps").first()
            assertEquals("if", branch.text("type"))
            assertEquals("storeValue", branch.list("then").single().obj("action").text("type"))
            assertTrue(branch.list("otherwise").isEmpty())
        }
    }

    @Test
    fun variablePickerInsertsATemplateWithoutTypingAnExpression() {
        var value by mutableStateOf(templateValue("Bonjour "))
        compose.setContent {
            CairnTheme {
                RuleValueEditor(
                    "Message",
                    value,
                    listOf(
                        RuleVariable(
                            "Événement",
                            "payload.author",
                            "Auteur",
                            "texte",
                            "La personne qui a écrit le message.",
                        )
                    ),
                    { value = it },
                )
            }
        }
        compose.onNodeWithText("+ Insérer une variable").performClick()
        compose.onNodeWithText("Utiliser Auteur").performClick()
        compose.runOnIdle { assertEquals("Bonjour {{payload.author}}", value.text("text")) }
    }

    @Test
    fun numericLiteralKeepsPartialDecimalInputWhileEditing() {
        var value by mutableStateOf(literalRuleValue(JsonPrimitive(1)))
        compose.setContent {
            CairnTheme {
                RuleValueEditor("Durée", value, emptyList(), { value = it }, expects = "number")
            }
        }
        compose.onNode(hasSetTextAction()).performTextReplacement("1.")
        compose.onNode(hasSetTextAction()).assertTextContains("1.")
        compose.onNode(hasSetTextAction()).performTextReplacement("1.25")
        compose.runOnIdle { assertEquals(1.25, value["value"]!!.jsonPrimitive.double, 0.0001) }
    }

    @Test
    fun fixedArrayItemsKeepTheirDeclaredBooleanType() {
        val schema =
            wireJson
                .parseToJsonElement("""{"type":"array","items":{"type":"boolean"}}""")
                .jsonObject
        var value by mutableStateOf<JsonElement>(JsonArray(emptyList()))
        compose.setContent {
            CairnTheme { RuleLiteralEditor(schema, value, { value = it }) }
        }
        compose.onNodeWithText("+ Ajouter un élément").performClick()
        compose.runOnIdle { assertEquals(JsonPrimitive(false), (value as JsonArray).single()) }
    }

    @Test
    fun variablesRespectBranchesAndEscapeUnusualPayloadNames() {
        val definition =
            wireJson.parseToJsonElement(
                """{"steps":[{"type":"if","id":"check","then":[{"type":"action","id":"first","action":{"type":"storeValue","name":"last"}}],"otherwise":[{"type":"action","id":"fallback","action":{"type":"storeValue","name":"last"}}]},{"type":"action","id":"after","action":{"type":"storeValue","name":"last"}}]}"""
            )
        assertFalse(
            ruleVariablesAt(definition, "/steps/0/otherwise/0", null).any {
                it.expression == "steps.first"
            }
        )
        assertTrue(
            ruleVariablesAt(definition, "/steps/1", null).any { it.expression == "steps.first" }
        )
        val schema = buildJsonObject {
            put(
                "properties",
                buildJsonObject { put("a\"b", buildJsonObject { put("type", "string") }) },
            )
        }
        assertEquals("payload[\"a\\\"b\"]", schemaFields(schema, "payload").single().expression)
    }

    @Test
    fun aWaitKeepsAnIntegerWireValueWhileEditing() {
        var value by mutableStateOf(literalRuleValue(JsonPrimitive(600)))
        compose.setContent {
            CairnTheme {
                RuleValueEditor("Durée", value, emptyList(), { value = it }, expects = "integer")
            }
        }
        compose.onNode(hasSetTextAction()).performTextReplacement("900")
        compose.runOnIdle { assertEquals("900", value["value"].toString()) }
    }

    @Test
    fun theFlowShowsErrorsAtTheirStep() {
        val step =
            wireJson.parseToJsonElement(
                """{"type":"action","id":"pause","action":{"type":"wait","seconds":{"type":"literal","value":0}}}"""
            )
        compose.setContent {
            CairnTheme {
                RuleFlowTree(
                    listOf(step),
                    "/steps",
                    null,
                    { "Assistant" },
                    editable = true,
                    problems =
                        listOf(
                            HookRuleProblem(
                                path = "/steps/0/action/seconds",
                                message = "Choisissez une durée positive.",
                            )
                        ),
                )
            }
        }
        compose
            .onNodeWithText("Choisissez une durée positive.", substring = true)
            .assertIsDisplayed()
    }
}
