package build.cairn.app.ui

import androidx.compose.runtime.*
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextRange
import build.cairn.app.data.*
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
class RuleInputReconciliationTest {
    @get:Rule val compose = createComposeRule(effectContext = StandardTestDispatcher())

    @Test
    fun aParentEchoCannotEraseTheNextFormulaKeystroke() {
        checkQueuedEcho(
            formulaRuleValue("initial"),
            formulaRuleValue("payload."),
            "payload.a",
            formulaRuleValue("payload.a"),
            "u",
            "payload.au",
            formulaRuleValue("payload.au"),
            "field",
        )
    }

    @Test
    fun aParentEchoCannotEraseTheNextNumericKeystroke() {
        checkQueuedEcho(
            literalRuleValue(JsonPrimitive(0)),
            literalRuleValue(JsonPrimitive(1)),
            "12",
            literalRuleValue(JsonPrimitive(12)),
            "3",
            "123",
            literalRuleValue(JsonPrimitive(123)),
            "number",
        )
    }

    @Test
    fun aParentEchoCannotEraseTheNextJsonKeystrokeOrItsInvalidDraft() {
        val text = """{"n":12}3"""
        checkQueuedEcho(
            literalRuleValue(wireJson.parseToJsonElement("""{"n":0}""")),
            literalRuleValue(wireJson.parseToJsonElement("""{"n":1}""")),
            """{"n":12}""",
            literalRuleValue(wireJson.parseToJsonElement("""{"n":12}""")),
            "3",
            text,
            literalWithJsonDrafts(JsonPrimitive(text), jsonInputDraft(text)),
            "json",
            ruleObject("type" to JsonPrimitive("object")),
        )
    }

    private fun checkQueuedEcho(
        initial: JsonObject,
        first: JsonObject,
        nextText: String,
        next: JsonObject,
        suffix: String,
        expectedText: String,
        expected: JsonObject,
        expects: String,
        schema: JsonObject? = null,
    ) {
        var value by mutableStateOf(initial)
        var replace: ((AnnotatedString) -> Boolean)? = null
        var insert: ((AnnotatedString) -> Boolean)? = null
        var phase = 0
        val injections = mutableListOf<String>()
        compose.setContent {
            val composed = value
            CairnTheme {
                RuleValueEditor(
                    "Value",
                    composed,
                    emptyList(),
                    { value = it },
                    expects,
                    schema = schema,
                )
            }
            // Input arrives after a composition has captured its prop, before
            // that composition's queued effects are drained. The next edit
            // makes a stale rollback observable rather than letting an echo
            // restore the final text and hide the lost IME connection.
            SideEffect {
                when {
                    phase == 0 && composed == first -> {
                        phase = 1
                        injections += "A"
                        checkNotNull(replace).invoke(AnnotatedString(nextText))
                    }
                    phase == 1 && composed == next -> {
                        phase = 2
                        injections += "B"
                        checkNotNull(insert).invoke(AnnotatedString(suffix))
                    }
                }
            }
        }
        val input = compose.onNode(hasSetTextAction())
        input.performClick()
        val semantics = input.fetchSemanticsNode().config
        replace = checkNotNull(semantics[SemanticsActions.SetText].action)
        insert = checkNotNull(semantics[SemanticsActions.InsertTextAtCursor].action)
        compose.runOnIdle { value = first }
        compose.waitForIdle()
        assertEquals(listOf("A", "B"), injections)
        input.assert(
            SemanticsMatcher.expectValue(
                SemanticsProperties.EditableText,
                AnnotatedString(expectedText),
            )
        )
        input.assert(
            SemanticsMatcher.expectValue(
                SemanticsProperties.TextSelectionRange,
                TextRange(expectedText.length),
            )
        )
        compose.runOnIdle { assertEquals(expected, value) }
    }
}
