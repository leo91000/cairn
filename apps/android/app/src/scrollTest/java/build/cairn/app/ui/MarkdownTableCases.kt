package build.cairn.app.ui

import android.text.Spanned
import android.view.View
import android.view.ViewGroup
import android.view.inspector.WindowInspector
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Surface
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.ComposeContentTestRule
import androidx.compose.ui.unit.dp
import io.noties.markwon.ext.tables.TableRowSpan
import org.junit.Assert.*

/** The same native rendering assertions run in Robolectric and on an Android device. */
internal class MarkdownTableCases(private val compose: ComposeContentTestRule) {
    fun readableColumns(nesting: String = "") {
        val columns = (1..12).joinToString(" | ") { "Column $it" }
        val values = (1..12).joinToString(" | ") { "Value $it" }
        val text =
            """
            The paragraph before the tables stays within the conversation.

            | Feature | What to verify |
            | --- | --- |
            | Waiting | Long descriptions wrap inside the cell and keep the surrounding message readable. |

            The paragraph between the tables also keeps the conversation width.

            | $columns |
            | ${(1..12).joinToString(" | ") { "---" }} |
            | $values |
        """
                .trimIndent()
        val markdown =
            when (nesting) {
                "quote" -> text.lines().joinToString("\n") { "> $it" }
                "list" ->
                    "1. ${text.lineSequence().first()}\n" +
                        text.lines().drop(1).joinToString("\n") { "   $it" }
                else -> text
            }
        compose.setContent {
            CairnTheme("dark") {
                Surface(Modifier.width(320.dp).fillMaxHeight()) {
                    Column(Modifier.verticalScroll(rememberScrollState())) { Markdown(markdown) }
                }
            }
        }
        fun views(): List<MarkdownTextView> {
            fun collect(view: View): List<MarkdownTextView> =
                when (view) {
                    is MarkdownTextView -> listOf(view)
                    is ViewGroup ->
                        (0 until view.childCount).flatMap { collect(view.getChildAt(it)) }
                    else -> emptyList()
                }
            return WindowInspector.getGlobalWindowViews().flatMap(::collect)
        }
        fun rows(view: MarkdownTextView): List<TableRowSpan> {
            val text = view.text as Spanned
            return text.getSpans(0, text.length, TableRowSpan::class.java).sortedBy {
                text.getSpanStart(it)
            }
        }
        repeat(3) {
            compose.onRoot().captureToImage()
            compose.waitForIdle()
        }
        compose.waitUntil(20000) {
            val tables = views().filter { rows(it).isNotEmpty() }
            tables.size == 2 &&
                tables.all { view ->
                    runCatching { rows(view).last().findLayoutForHorizontalOffset(0) }
                        .getOrNull() != null
                }
        }
        compose.runOnIdle {
            val all = views()
            val prose = all.filter { rows(it).isEmpty() }
            val tables = all.filter { rows(it).isNotEmpty() }
            assertEquals(2, prose.size)
            assertTrue(prose.all { it.width == tables.first().width })
            assertTrue(tables.last().width > tables.first().width * 3)
            val row = rows(tables.first()).last()
            val description = row.findLayoutForHorizontalOffset(row.cellWidth() + 1)!!
            assertTrue(
                "Cell=${description.text}, width=${description.width}, view=${tables.first().width}, cell=${row.cellWidth()}, font=${tables.first().textSize}",
                description.lineCount > 1,
            )
            assertEquals(tables.first().textSize, tables.last().textSize, 0.1f)
            assertTrue(rows(tables.last()).last().cellWidth() >= tables.last().textSize * 5)
        }
        val wide = compose.onAllNodesWithTag("markdown-table").onLast()
        wide.performScrollTo()
        wide.performTouchInput { swipeLeft() }
        compose.waitForIdle()
        val scroll = wide.fetchSemanticsNode().config[SemanticsProperties.HorizontalScrollAxisRange]
        assertTrue(
            "Wide tables must scroll without widening surrounding prose",
            scroll.value() > 0f,
        )
        compose.runOnIdle {
            val row = rows(views().last()).last()
            assertTrue(
                row.findLayoutForHorizontalOffset(row.cellWidth() * 11 + 1)!!
                    .text
                    .contains("Value 12")
            )
        }
    }
}
