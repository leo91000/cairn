package build.cairn.app.ui

import android.app.Application
import android.net.Uri
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.text.TextRange
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import build.cairn.contracts.HookRuleProblem
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.*
import org.junit.*
import org.junit.Assert.*

abstract class RulesEditorCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun aLateResourcePageCannotReplaceANewSearch() {
        val requests = CopyOnWriteArrayList<String>()
        val late = CompletableDeferred<JsonObject>()
        val returned = AtomicBoolean(false)
        var selected: JsonObject? = null
        fun page(id: String, next: JsonElement) =
            ruleObject(
                "items" to
                    JsonArray(
                        listOf(
                            ruleObject(
                                "resourceType" to JsonPrimitive("person"),
                                "id" to JsonPrimitive(id),
                                "name" to JsonPrimitive(id),
                            )
                        )
                    ),
                "nextPage" to next,
            )
        compose.setContent {
            CairnTheme {
                ScopedRuleResourceSheet(
                    RuleResourceSource("team", "messages", "person", "agent"),
                    { path ->
                        requests += path
                        val uri = Uri.parse(path)
                        when {
                            uri.getQueryParameter("query") == "new" -> page("New result", JsonNull)
                            uri.getQueryParameter("page") == "2" ->
                                withContext(NonCancellable) {
                                    val result = late.await()
                                    returned.set(true)
                                    result
                                }
                            else -> page("First result", JsonPrimitive(2))
                        }
                    },
                    { selected = it },
                    {},
                )
            }
        }
        try {
            compose.waitUntil(5000) { requests.isNotEmpty() }
            compose.onNodeWithText("Voir la suite").performScrollTo().performClick()
            compose.waitUntil(5000) {
                requests.any { Uri.parse(it).getQueryParameter("page") == "2" }
            }
            compose.onNode(hasSetTextAction()).performTextReplacement("new")
            compose.waitUntil(5000) {
                compose.onAllNodesWithText("New result").fetchSemanticsNodes().isNotEmpty()
            }
            late.complete(page("Old result", JsonPrimitive(3)))
            compose.waitUntil(5000) { returned.get() }
            compose.waitForIdle()
            compose.onNodeWithText("Old result").assertDoesNotExist()
            compose.onNodeWithText("Voir la suite").assertDoesNotExist()
            compose.onNodeWithText("New result").performClick()
            compose.runOnIdle { assertEquals("New result", selected.text("id")) }
        } finally {
            late.complete(page("Old result", JsonPrimitive(3)))
        }
    }

    @Test fun aListAtTheDepthLimitRemainsJson() = editDeepList(false)

    @Test fun editingADeepNestedListPropagatesItsArrayValue() = editDeepList(true)

    private fun editDeepList(nested: Boolean) {
        var schema =
            ruleObject(
                "type" to JsonPrimitive("array"),
                "items" to ruleObject("type" to JsonPrimitive("integer")),
            )
        var initial: JsonElement = JsonArray(listOf(JsonPrimitive(1), JsonPrimitive(2)))
        var expected: JsonElement = JsonArray(listOf(JsonPrimitive(3), JsonPrimitive(4)))
        if (nested)
            repeat(6) {
                schema = ruleObject("type" to JsonPrimitive("array"), "items" to schema)
                initial = JsonArray(listOf(initial))
                expected = JsonArray(listOf(expected))
            }
        var value by mutableStateOf(initial)
        compose.setContent {
            CairnTheme {
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    RuleLiteralEditor(schema, value, { value = it }, depth = if (nested) 0 else 6)
                }
            }
        }
        val input = compose.onNode(hasSetTextAction())
        input.performScrollTo().assertTextContains("[1,2]")
        input.performTextReplacement("[")
        input.assertTextContains("[")
        input.performTextReplacement("[3,4]")
        compose.runOnIdle { assertEquals(expected, value) }
    }

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
    fun editingAnEqualityFieldPreservesTheRightTemplate() {
        val fields =
            schemaFields(
                wireJson.parseToJsonElement("""{"properties":{"channel":{"type":"string"}}}"""),
                "payload",
            )
        for (text in listOf("support-{{payload.author}}", "{{payload.author}}", "3")) {
            var right = templateValue(text)
            for (expression in
                listOf(
                    "payload.channel",
                    "payload.channe",
                    "payload.channel",
                    "steps.fetch.name",
                )) {
                for (operator in listOf("equals", "notEquals")) {
                    right =
                        typedComparisonRight(
                            right,
                            comparisonExpects(formulaRuleValue(expression), operator, fields),
                            listOnly = false,
                        )
                    assertEquals(templateValue(text), right)
                }
            }
        }
    }

    @Test
    fun loopItemComparisonsPreserveLiteralTypes() {
        val definition =
            ruleObject(
                "steps" to
                    JsonArray(
                        listOf(
                            ruleObject(
                                "type" to JsonPrimitive("forEach"),
                                "itemName" to JsonPrimitive("entry"),
                                "items" to literalRuleValue(JsonArray(listOf(JsonPrimitive(5)))),
                                "body" to
                                    JsonArray(listOf(ruleObject("type" to JsonPrimitive("if")))),
                            )
                        )
                    )
            )
        val variables = ruleVariablesAt(definition, "/steps/0/body/0", null)
        for (literal in
            listOf(
                JsonPrimitive(5),
                JsonPrimitive(true),
                ruleObject("count" to JsonPrimitive(5)),
            )) {
            var right = literalRuleValue(literal)
            for (operator in listOf("notEquals", "equals")) {
                right =
                    typedComparisonRight(
                        right,
                        comparisonExpects(formulaRuleValue("entry"), operator, variables),
                        listOnly = false,
                    )
                assertEquals(literalRuleValue(literal), right)
            }
        }
    }

    @Test
    fun aSchemaStringReplacesTheSelectedTextWhenInsertingAVariable() =
        insertSchemaStringVariable(false)

    @Test
    fun aNullableSchemaStringReplacesTheSelectedTextWhenInsertingAVariable() =
        insertSchemaStringVariable(true)

    @Test
    fun aFreeSchemaStringReplacesTheSelectedTextWhenInsertingAVariable() =
        insertSchemaStringVariable(false, true)

    @Test
    fun aMixedSchemaStringReplacesTheSelectedTextWhenInsertingAVariable() =
        insertSchemaStringVariable(false, generic = true)

    private fun insertSchemaStringVariable(
        nullable: Boolean,
        untyped: Boolean = false,
        generic: Boolean = false,
    ) {
        val changes = mutableListOf<JsonObject>()
        var value by mutableStateOf(literalRuleValue(JsonPrimitive("Hello Alice!")))
        compose.setContent {
            CairnTheme {
                RuleValueEditor(
                    "Message",
                    value,
                    listOf(RuleVariable("Événement", "payload.author", "Auteur", "string", "")),
                    {
                        changes += it
                        value = it
                    },
                    schema =
                        if (untyped) JsonObject(emptyMap())
                        else
                            ruleObject(
                                "type" to
                                    if (generic)
                                        JsonArray(
                                            listOf(JsonPrimitive("number"), JsonPrimitive("string"))
                                        )
                                    else if (nullable)
                                        JsonArray(
                                            listOf(JsonPrimitive("string"), JsonPrimitive("null"))
                                        )
                                    else JsonPrimitive("string"),
                                "maxLength" to JsonPrimitive(300),
                            ),
                )
            }
        }
        compose
            .onNode(hasSetTextAction())
            .performClick()
            .performTextInputSelection(
                if (untyped || generic) TextRange(7, 12) else TextRange(6, 11)
            )
        compose.onNodeWithText("+ Insérer une variable").performClick()
        val use = compose.onNodeWithText("Utiliser Auteur")
        // Android's IME/window insets can still move a dialog after Compose is
        // idle. Wait for the actual touch target to settle before tapping it.
        var position = use.fetchSemanticsNode().boundsInWindow
        var settledAt = System.nanoTime()
        compose.waitUntil(3000) {
            val current = use.fetchSemanticsNode().boundsInWindow
            if (current != position) {
                position = current
                settledAt = System.nanoTime()
            }
            current.width > 0 && current.height > 0 && System.nanoTime() - settledAt >= 200_000_000L
        }
        use.assertIsDisplayed()
        use.performClick()
        val remaining =
            compose.onAllNodesWithText("Utiliser Auteur").fetchSemanticsNodes().map {
                it.boundsInRoot
            }
        compose.runOnIdle {
            assertEquals(
                "changes=$changes remaining=$remaining",
                templateValue("Hello {{payload.author}}!"),
                value,
            )
        }
    }

    @Test
    fun editingALiteralStringInAFieldKeepsItsQuotes() {
        var value by mutableStateOf(literalRuleValue(JsonPrimitive("fixed")))
        compose.setContent {
            CairnTheme {
                RuleValueEditor("Champ", value, emptyList(), { value = it }, expects = "field")
            }
        }
        val input = compose.onNode(hasSetTextAction())
        input.assertTextContains("\"fixed\"")
        input.performTextReplacement("\"kept\"")
        compose.runOnIdle { assertEquals(formulaRuleValue("\"kept\""), value) }
    }

    @Test
    fun nestedPropertiesDistinguishMissingNullAndFalseAndKeepUnrelatedEdits() {
        val schema =
            resolveRuleSchema(
                wireJson
                    .parseToJsonElement(
                        """{"type":"object","properties":{"notify":{"title":"Notify","type":["boolean","null"],"default":null},"caption":{"title":"Caption","type":"string"}}}"""
                    )
                    .jsonObject
            )
        var value by mutableStateOf<JsonElement>(ruleObject("notify" to JsonNull))
        compose.setContent {
            CairnTheme {
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    RuleLiteralEditor(schema, value, { value = it })
                }
            }
        }
        compose.onNodeWithText("Valeur nulle").assertIsDisplayed()
        compose.onNodeWithText("Non renseigné.").assertIsDisplayed()
        compose.onAllNodes(isToggleable()).assertCountEquals(0)
        compose.runOnIdle { assertEquals(ruleObject("notify" to JsonNull), value) }
        compose.onNodeWithText("+ Caption").performScrollTo().performClick()
        compose.onNode(hasSetTextAction()).performScrollTo().performTextReplacement("kept")
        compose.runOnIdle {
            assertEquals(
                ruleObject("notify" to JsonNull, "caption" to JsonPrimitive("kept")),
                value,
            )
        }
        compose.onNodeWithText("Définir une valeur").performScrollTo().performClick()
        compose.onNode(isToggleable()).assertIsOff()
        compose.runOnIdle { assertEquals(JsonPrimitive(false), value.jsonObject["notify"]) }
        compose.onNodeWithText("Utiliser une valeur nulle").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(JsonNull, value.jsonObject["notify"]) }
        compose.onNodeWithText("Retirer le champ Notify").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(ruleObject("caption" to JsonPrimitive("kept")), value) }
        compose.onNodeWithText("+ Notify").performScrollTo().performClick()
        compose.runOnIdle {
            assertEquals(
                ruleObject("notify" to JsonNull, "caption" to JsonPrimitive("kept")),
                value,
            )
        }
    }

    @Test
    fun nullableTextShowsNullWithoutPresentingTheStringNull() {
        var value by mutableStateOf(literalRuleValue(JsonNull))
        val schema =
            wireJson.parseToJsonElement("""{"type":["string","null"],"default":null}""").jsonObject
        compose.setContent {
            CairnTheme {
                RuleValueEditor("Caption", value, emptyList(), { value = it }, schema = schema)
            }
        }
        compose.onNodeWithText("Valeur nulle").assertIsDisplayed()
        compose.onAllNodes(hasSetTextAction()).assertCountEquals(0)
        compose.onNodeWithText("Définir une valeur").performClick()
        compose.runOnIdle { assertEquals(literalRuleValue(JsonPrimitive("")), value) }
        compose.onNode(hasSetTextAction()).performTextReplacement("kept")
        compose.onNodeWithText("Utiliser une valeur nulle").performClick()
        compose.runOnIdle { assertEquals(literalRuleValue(JsonNull), value) }
    }

    @Test fun malformedJsonKeepsItsDraftUntilFixed() = recoverInvalidValue(false)

    @Test
    fun constantModeRoundTripsPreserveValuesAndExplainReplacingAnExpression() {
        var value by mutableStateOf(literalRuleValue(JsonPrimitive(false)))
        var expects by mutableStateOf("boolean")
        compose.setContent {
            CairnTheme {
                RuleValueEditor("Constante", value, emptyList(), { value = it }, expects = expects)
            }
        }
        for ((kind, fixed) in
            listOf(
                "boolean" to JsonPrimitive(false),
                "text" to JsonPrimitive("fixed"),
                "number" to JsonPrimitive(1.5),
                "json" to JsonNull,
                "json" to JsonArray(listOf(JsonPrimitive(1))),
            )) {
            compose.runOnIdle {
                expects = kind
                value = literalRuleValue(fixed)
            }
            compose.onNodeWithText("Formule").performClick()
            compose.onNodeWithText("Valeur").performClick()
            compose.runOnIdle { assertEquals(literalRuleValue(fixed), value) }
        }
        compose.runOnIdle {
            expects = "boolean"
            value = formulaRuleValue("payload.active")
        }
        compose.onNodeWithText("Valeur").performClick()
        compose
            .onNodeWithText(
                "La formule a été remplacée par une valeur fixe. Vérifiez cette valeur avant d’enregistrer."
            )
            .assertIsDisplayed()
    }

    @Test fun malformedSchemaJsonKeepsItsDraftUntilFixed() = recoverInvalidValue(true)

    private fun recoverInvalidValue(schemaInput: Boolean) {
        val raw = "{\"count\":"
        var value by mutableStateOf(literalRuleValue(ruleObject("count" to JsonPrimitive(1))))
        var shown by mutableStateOf(true)
        compose.setContent {
            CairnTheme {
                if (shown)
                    RuleValueEditor(
                        "Valeur",
                        value,
                        emptyList(),
                        { value = it },
                        expects = if (schemaInput) "text" else "json",
                        schema = if (schemaInput) JsonObject(emptyMap()) else null,
                    )
            }
        }
        compose.onNode(hasSetTextAction()).performTextReplacement(raw)
        compose.runOnIdle {
            assertEquals(1, ruleJsonDraftProblems(value).size)
            value = wireJson.parseToJsonElement(value.toString()).jsonObject
            shown = false
        }
        compose.runOnIdle { shown = true }
        compose.onNode(hasSetTextAction()).assertTextContains(raw)
        compose.onNode(hasSetTextAction()).performTextReplacement(JsonPrimitive(raw).toString())
        compose.runOnIdle {
            assertTrue(ruleJsonDraftProblems(value).isEmpty())
            assertEquals(literalRuleValue(JsonPrimitive(raw)), value)
        }
    }

    @Test
    fun malformedNumericComparisonKeepsItsDraftAndBlocksValidation() {
        var condition by
            mutableStateOf(
                ruleObject(
                    "type" to JsonPrimitive("compare"),
                    "left" to formulaRuleValue("payload.count"),
                    "operator" to JsonPrimitive("greaterThan"),
                    "right" to literalRuleValue(JsonPrimitive(1)),
                )
            )
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
                CairnTheme {
                    Column(Modifier.verticalScroll(rememberScrollState())) {
                        RuleConditionEditor(condition, emptyList(), { condition = it }, vm, null)
                    }
                }
            }
            val right =
                compose.onNode(hasSetTextAction() and hasContentDescription("Comparer avec"))
            right.performScrollTo().performTextReplacement("1.5e")
            compose.runOnIdle { assertEquals(1, ruleJsonDraftProblems(condition).size) }
            compose.onNodeWithText("est supérieur à").performScrollTo().performClick()
            compose.onNodeWithText("est inférieur à").performClick()
            compose
                .onNode(hasSetTextAction() and hasContentDescription("Champ à vérifier"))
                .performScrollTo()
                .performTextReplacement("payload.count + 0")
            right.performScrollTo().assertTextContains("1.5e")
            compose.runOnIdle { assertEquals(1, ruleJsonDraftProblems(condition).size) }
            right.performTextReplacement("3")
            compose.runOnIdle {
                assertTrue(ruleJsonDraftProblems(condition).isEmpty())
                assertEquals(literalRuleValue(JsonPrimitive(3)), condition["right"])
            }
        } finally {
            ui.close()
        }
    }

    @Test
    fun jsonStringEditingKeepsItsTypeAndTextModeHasNoEncodingQuotes() {
        var value by mutableStateOf(literalRuleValue(JsonPrimitive("5")))
        compose.setContent {
            CairnTheme {
                RuleValueEditor(
                    "Valeur mémorisée",
                    value,
                    emptyList(),
                    { value = it },
                    expects = "json",
                )
            }
        }
        val input = compose.onNode(hasSetTextAction())
        input.assertTextContains("\"5\"")
        input.performTextInputSelection(TextRange(2))
        input.performTextInput("6")
        compose.runOnIdle { assertEquals(literalRuleValue(JsonPrimitive("56")), value) }
        compose.onNodeWithText("Texte").performClick()
        compose.runOnIdle { assertEquals(templateValue("56"), value) }
        input.assertTextContains("56")
    }

    @Test
    fun jsonValueEditorKeepsPartialInputBeforeAnObjectIsComplete() {
        var value by mutableStateOf(literalRuleValue(JsonPrimitive("")))
        compose.setContent {
            CairnTheme {
                RuleValueEditor(
                    "Valeur mémorisée",
                    value,
                    emptyList(),
                    { value = it },
                    expects = "json",
                )
            }
        }
        val input = compose.onNode(hasSetTextAction())
        input.performTextClearance()
        var expected = ""
        for (character in "{\"n\":1.5}") {
            expected += character
            input.performTextInput(character.toString())
            input.assertTextContains(expected)
        }
        compose.runOnIdle {
            assertEquals(literalRuleValue(wireJson.parseToJsonElement("""{"n":1.5}""")), value)
        }
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
