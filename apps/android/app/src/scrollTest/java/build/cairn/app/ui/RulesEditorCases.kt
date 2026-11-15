package build.cairn.app.ui

import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.text.TextRange
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
    fun nestedLoopsHaveUniqueNamesAndTheirVariablePickerDoesNotCrash() {
        val empty = ruleObject("steps" to JsonArray(emptyList()))
        val outer = newRuleStep(empty, "forEach", null)
        val first = empty.inserted("/steps", 0, outer)
        val inner = newRuleStep(first, "forEach", null)
        assertEquals("item", outer.text("itemName"))
        assertEquals("item2", inner.text("itemName"))
        // Old drafts may already contain shadowed names; the innermost one wins.
        val legacy =
            first.inserted("/steps/0/body", 0, inner.patch("itemName" to JsonPrimitive("item")))
        val variables = ruleVariablesAt(legacy, "/steps/0/body/0/body/0", null)
        assertEquals(1, variables.count { it.expression == "item" })
        var value by mutableStateOf(templateValue())
        compose.setContent {
            CairnTheme { RuleValueEditor("Message", value, variables, { value = it }) }
        }
        compose.onNodeWithText("+ Insérer une variable").performClick()
        compose.onNodeWithText("Utiliser item").performClick()
        compose.runOnIdle { assertEquals("{{item}}", value.text("text")) }
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
    fun freeObjectsCanBeEditedAsJsonAndKeepInvalidDraftsVisible() {
        val schema = ruleObject("type" to JsonPrimitive("object"))
        var value by mutableStateOf<JsonElement>(JsonObject(emptyMap()))
        compose.setContent { CairnTheme { RuleLiteralEditor(schema, value, { value = it }) } }
        compose.onNodeWithText("Objet JSON").performTextReplacement("{invalid")
        compose.onNodeWithText("Saisissez un objet JSON valide.").assertIsDisplayed()
        compose.runOnIdle { assertEquals(JsonPrimitive("{invalid"), value) }
        compose
            .onNodeWithText("Objet JSON")
            .performTextReplacement("""{"to":["a@example.test"],"count":5}""")
        compose.runOnIdle { assertEquals(JsonPrimitive(5), value.jsonObject["count"]) }
    }

    @Test
    fun authorsAndListMembersUseTheirDeclaredTypes() {
        val schema =
            wireJson.parseToJsonElement(
                """{"type":"object","properties":{"author":{"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string"}}},"counts":{"type":"array","items":{"type":"integer"}},"flags":{"type":"array","items":{"type":"boolean"}}}}"""
            )
        val fields = schemaFields(schema, "payload")
        assertEquals("payload.author.id", authorVariable(fields))
        for (name in listOf("payload.counts", "counts")) assertEquals(
            "number",
            comparisonExpects(formulaRuleValue(name), "contains", fields),
        )
        assertEquals(
            "boolean",
            comparisonExpects(formulaRuleValue("payload.flags"), "contains", fields),
        )
        assertEquals(
            literalRuleValue(JsonPrimitive(5)),
            typedComparisonRight(literalRuleValue(JsonPrimitive(5)), "json", listOnly = false),
        )
    }

    @Test
    fun equalityKeepsObjectsAndListsStructured() {
        val fields =
            schemaFields(
                wireJson.parseToJsonElement(
                    """{"properties":{"items":{"type":"array"},"meta":{"type":"object"}}}"""
                ),
                "payload",
            )
        for ((name, value) in
            listOf(
                "items" to wireJson.parseToJsonElement("[1,2]"),
                "meta" to wireJson.parseToJsonElement("""{"ok":true}"""),
            )) {
            for (operator in listOf("equals", "notEquals")) {
                val expected =
                    comparisonExpects(formulaRuleValue("payload.$name"), operator, fields)
                assertEquals("json", expected)
                assertEquals(
                    literalRuleValue(value),
                    typedComparisonRight(literalRuleValue(value), expected, listOnly = false),
                )
            }
        }
    }

    @Test
    fun unknownFormulaEqualityKeepsNumbersAndNestedMembershipKeepsItemTypes() {
        for (expression in listOf("size(payload.items)", "steps.fetch.count")) {
            for (operator in listOf("equals", "notEquals")) {
                val expected =
                    comparisonExpects(formulaRuleValue(expression), operator, emptyList())
                assertEquals("json", expected)
                assertEquals(
                    literalRuleValue(JsonPrimitive(3)),
                    typedComparisonRight(
                        literalRuleValue(JsonPrimitive(3)),
                        expected,
                        listOnly = false,
                    ),
                )
            }
        }
        val fields =
            schemaFields(
                wireJson.parseToJsonElement(
                    """{"properties":{"counts":{"type":"array","items":{"type":"integer"}}}}"""
                ),
                "payload",
            )
        val schema = comparisonListSchema(formulaRuleValue("payload.counts"), fields)
        assertEquals("integer", schema.obj("items")?.obj("items").text("type"))
        var value by mutableStateOf<JsonElement>(wireJson.parseToJsonElement("[[1,2]]"))
        compose.setContent { CairnTheme { RuleLiteralEditor(schema, value, { value = it }) } }
        compose.onAllNodes(hasSetTextAction())[0].performTextReplacement("5")
        compose.runOnIdle { assertEquals(wireJson.parseToJsonElement("[[5,2]]"), value) }
    }

    @Test
    fun untypedJsonKeepsPartialTextWhileTypingAnObject() {
        var value by mutableStateOf<JsonElement>(JsonObject(emptyMap()))
        compose.setContent {
            CairnTheme { RuleLiteralEditor(JsonObject(emptyMap()), value, { value = it }) }
        }
        val input = compose.onNode(hasSetTextAction())
        input.performTextClearance()
        var expected = ""
        for (character in "{\"n\":1.5}") {
            expected += character
            input.performTextInput(character.toString())
            input.assertTextContains(expected)
        }
        compose.runOnIdle { assertEquals(wireJson.parseToJsonElement("""{"n":1.5}"""), value) }
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
        compose.onNodeWithText("Formule").performClick()
        compose.onNodeWithText("Valeur").performClick()
        compose.runOnIdle { assertEquals(JsonPrimitive(900), value["value"]) }
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

    @Test
    fun comparisonFieldsKeepNumbersAndBooleansTyped() {
        val variables =
            listOf(
                RuleVariable("Événement", "payload.count", "count", "integer", ""),
                RuleVariable("Événement", "payload.approved", "approved", "boolean", ""),
            )
        for (expression in listOf("steps.fetch.count", "size(payload.items)")) {
            for (operator in listOf("greaterThan", "lessThan")) assertEquals(
                "number",
                comparisonExpects(formulaRuleValue(expression), operator, emptyList()),
            )
        }
        val left = formulaRuleValue("payload.count")
        assertEquals("number", comparisonExpects(left, "greaterThan", variables))
        assertEquals(
            "boolean",
            comparisonExpects(formulaRuleValue("approved"), "equals", variables),
        )
        var number by mutableStateOf(typedComparisonRight(templateValue(), "number"))
        var flag by mutableStateOf(typedComparisonRight(templateValue(), "boolean"))
        compose.setContent {
            CairnTheme {
                androidx.compose.foundation.layout.Column {
                    RuleValueEditor(
                        "Nombre",
                        number,
                        variables,
                        { number = it },
                        expects = "number",
                    )
                    RuleValueEditor("Accord", flag, variables, { flag = it }, expects = "boolean")
                }
            }
        }
        compose.onNode(hasSetTextAction()).performTextReplacement("5")
        compose.onNodeWithText("Oui", useUnmergedTree = true).performClick()
        compose.runOnIdle {
            assertEquals(JsonPrimitive(5), number["value"])
            assertEquals(JsonPrimitive(true), flag["value"])
            assertEquals(
                "formula",
                typedComparisonRight(templateValue("{{payload.count}}"), "number").text("type"),
            )
        }
    }

    @Test
    fun functionChoicesShowTheirSignatureAndExampleAndInsertAnExpression() {
        val example = "truncate(payload.content, 80)"
        val signature = "truncate(text: string, length: int) -> string"
        var value by mutableStateOf(templateValue())
        compose.setContent {
            CairnTheme {
                RuleValueEditor(
                    "Message",
                    value,
                    listOf(
                        RuleVariable(
                            "Fonctions",
                            example,
                            signature,
                            "ƒ",
                            "Raccourcir le texte.",
                            example,
                        )
                    ),
                    { value = it },
                )
            }
        }
        compose.onNodeWithText("+ Insérer une variable").performClick()
        compose.onNodeWithText(signature).assertIsDisplayed()
        compose.onNodeWithText(example).assertIsDisplayed()
        compose.onNodeWithText("Utiliser $signature").performClick()
        compose.runOnIdle { assertEquals("{{$example}}", value.text("text")) }
    }

    @Test
    fun anAgentWithoutToolsDoesNotOfferAnInvalidPluginAction() {
        compose.setContent {
            CairnTheme {
                RuleFlowTree(
                    emptyList(),
                    "/steps",
                    null,
                    { "Assistant" },
                    editable = true,
                    executionAgentId = MAIN_AGENT_ID,
                )
            }
        }
        compose.onNodeWithText("Étape").performClick()
        compose.onNodeWithText("Appeler un outil").assertDoesNotExist()
        compose.onNodeWithText("Si / sinon").assertIsDisplayed()
    }

    @Test
    fun typedVariableInsertionProducesAFormulaInsteadOfText() {
        var count by mutableStateOf(literalRuleValue(JsonPrimitive(0)))
        var approved by mutableStateOf(literalRuleValue(JsonPrimitive(false)))
        val variables =
            listOf(
                RuleVariable("Événement", "payload.count", "Nombre", "integer", ""),
                RuleVariable("Événement", "payload.approved", "Accord", "boolean", ""),
            )
        compose.setContent {
            CairnTheme {
                androidx.compose.foundation.layout.Column {
                    RuleValueEditor("Nombre", count, variables, { count = it }, expects = "number")
                    RuleValueEditor(
                        "Accord",
                        approved,
                        variables,
                        { approved = it },
                        expects = "boolean",
                    )
                }
            }
        }
        compose.onAllNodesWithText("+ Insérer une variable")[0].performClick()
        compose.onNodeWithText("Utiliser Nombre").performClick()
        compose.onAllNodesWithText("+ Insérer une variable")[1].performClick()
        compose.onNodeWithText("Utiliser Accord").performClick()
        compose.runOnIdle {
            assertEquals(formulaRuleValue("payload.count"), count)
            assertEquals(formulaRuleValue("payload.approved"), approved)
        }
    }

    @Test
    fun formulaInsertionUsesTheCaretAndPreservesItsSurroundings() {
        var value by mutableStateOf(formulaRuleValue("payload.count >  && true"))
        compose.setContent {
            CairnTheme {
                RuleValueEditor(
                    "Condition",
                    value,
                    listOf(RuleVariable("Événement", "payload.count", "Nombre", "integer", "")),
                    { value = it },
                    expects = "formula",
                )
            }
        }
        compose.onNode(hasSetTextAction()).performClick().performTextInputSelection(TextRange(16))
        compose.onNodeWithText("+ Insérer une variable").performClick()
        compose.onNodeWithText("Utiliser Nombre").performClick()
        compose.runOnIdle {
            assertEquals(formulaRuleValue("payload.count > payload.count && true"), value)
        }
    }

    @Test
    fun numberMembershipChoicesStoreNumbersInsteadOfStrings() {
        val variables = listOf(RuleVariable("Événement", "payload.count", "Nombre", "integer", ""))
        val schema = comparisonListSchema(formulaRuleValue("payload.count"), variables)
        var value by mutableStateOf<JsonElement>(JsonArray(emptyList()))
        compose.setContent { CairnTheme { RuleLiteralEditor(schema, value, { value = it }) } }
        compose.onNodeWithText("+ Ajouter un élément").performClick()
        compose.onNode(hasSetTextAction()).performTextReplacement("8")
        compose.runOnIdle { assertEquals(JsonArray(listOf(JsonPrimitive(8))), value) }
        assertEquals(
            "boolean",
            comparisonListSchema(literalRuleValue(JsonPrimitive(true)), emptyList())
                .obj("items")
                .text("type"),
        )
    }

    @Test
    fun untypedJsonTextKeepsInterpolationAndItsPrefix() {
        var value by mutableStateOf(literalRuleValue(JsonPrimitive("Avant ")))
        compose.setContent {
            CairnTheme {
                RuleValueEditor(
                    "Valeur mémorisée",
                    value,
                    listOf(RuleVariable("Événement", "payload.author", "Auteur", "string", "")),
                    { value = it },
                    expects = "json",
                )
            }
        }
        compose.onNodeWithText("Valeur").performClick()
        compose.onNodeWithText("+ Insérer une variable").performClick()
        compose.onNodeWithText("Utiliser Auteur").performClick()
        compose.runOnIdle { assertEquals(templateValue("Avant {{payload.author}}"), value) }
    }

    @Test
    fun switchingATemplateWithTwoInsertionsToAFormulaKeepsValidQuotedText() {
        val template = "{{payload.a}}-{{payload.b}}"
        var value by mutableStateOf(templateValue(template))
        compose.setContent {
            CairnTheme { RuleValueEditor("Texte", value, emptyList(), { value = it }) }
        }
        compose.onNodeWithText("Formule").performClick()
        compose.runOnIdle {
            assertEquals(formulaRuleValue(JsonPrimitive(template).toString()), value)
            assertEquals("payload.a", unwrapRuleTemplate(" {{payload.a}} "))
        }
    }

    @Test
    fun aRememberedJsonValueKeepsItsNumberWhenChoosingFixedValue() {
        var value by mutableStateOf(templateValue("5"))
        compose.setContent {
            CairnTheme {
                RuleValueEditor("Mémoire", value, emptyList(), { value = it }, expects = "json")
            }
        }
        compose.onNodeWithText("Valeur").performClick()
        compose.runOnIdle { assertEquals(literalRuleValue(JsonPrimitive(5)), value) }
    }

    @Test
    fun comparisonTextValuesFollowFieldChangesInBothDirections() {
        assertEquals(
            literalRuleValue(JsonPrimitive("5")),
            typedComparisonRight(literalRuleValue(JsonPrimitive(5)), "text"),
        )
        assertEquals(
            literalRuleValue(JsonPrimitive("true")),
            typedComparisonRight(literalRuleValue(JsonPrimitive(true)), "text"),
        )
        val list = JsonArray(listOf(JsonPrimitive("alice"), JsonPrimitive("bob")))
        assertEquals(
            literalRuleValue(JsonPrimitive(list.toString())),
            typedComparisonRight(literalRuleValue(list), "text"),
        )
    }

    @Test
    fun schemaObjectFieldsOfferValuesAndFormulasWithoutTextMode() {
        var value by mutableStateOf(literalRuleValue(JsonObject(emptyMap())))
        compose.setContent {
            CairnTheme {
                RuleValueEditor(
                    "Options",
                    value,
                    emptyList(),
                    { value = it },
                    expects = "json",
                    schema =
                        ruleObject(
                            "type" to JsonPrimitive("object"),
                            "properties" to JsonObject(emptyMap()),
                        ),
                )
            }
        }
        compose.onNodeWithText("Texte", useUnmergedTree = true).assertDoesNotExist()
        compose.onNodeWithText("Formule").assertIsDisplayed()
    }

    @Test
    fun schemaTextKeepsItsContentsWhenSwitchingFromTextToValue() {
        val text = "Received: {{content}}"
        var value by mutableStateOf(templateValue(text))
        compose.setContent {
            CairnTheme {
                RuleValueEditor(
                    "Message",
                    value,
                    emptyList(),
                    { value = it },
                    schema = ruleObject("type" to JsonPrimitive("string")),
                )
            }
        }
        compose.onNodeWithText("Valeur").performClick()
        compose.runOnIdle { assertEquals(literalRuleValue(JsonPrimitive(text)), value) }
    }
}
