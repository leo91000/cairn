package build.cairn.app.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.text.Spannable
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.method.LinkMovementMethod
import android.text.style.LeadingMarginSpan
import android.view.ActionMode
import android.view.Menu
import android.view.MenuItem
import android.view.MotionEvent
import android.view.View
import android.widget.TextView
import androidx.compose.runtime.*
import build.cairn.app.diagnostics.PerfOperation
import build.cairn.app.diagnostics.Performance
import io.noties.markwon.Markwon
import io.noties.markwon.core.spans.BulletListItemSpan
import io.noties.markwon.core.spans.OrderedListItemSpan
import io.noties.markwon.ext.tables.TableAwareMovementMethod
import io.noties.markwon.ext.tables.TableSpan
import kotlinx.coroutines.flow.first
import org.commonmark.ext.gfm.strikethrough.StrikethroughExtension
import org.commonmark.ext.gfm.tables.TableBlock
import org.commonmark.ext.gfm.tables.TableCell
import org.commonmark.ext.gfm.tables.TablesExtension
import org.commonmark.node.AbstractVisitor
import org.commonmark.node.CustomBlock
import org.commonmark.node.Document
import org.commonmark.renderer.html.HtmlRenderer

/** A rendered group is immutable and shared with the UI until its parsed content changes. */
internal class MarkdownBlock(val signature: String, val text: Spanned, val tableColumns: Int = 0)

/**
 * Parse the full document so references, unfinished fences and lists stay correct. Render small
 * groups of complete top-level nodes, retaining unchanged groups. HTML is only a canonical AST
 * fingerprint (never displayed or loaded in a WebView). Each instance belongs to one sequential
 * background collector.
 */
internal class MarkdownBlocks(private val markwon: Markwon) {
    private val signatures =
        HtmlRenderer.builder()
            .extensions(listOf(TablesExtension.create(), StrikethroughExtension.create()))
            .build()

    private class Group(val signature: String, val blocks: List<MarkdownBlock>)

    private var previous = emptyList<Group>()

    fun render(content: String): List<MarkdownBlock> =
        Performance.measure(PerfOperation.MARKDOWN) {
            val document = markwon.parse(content)
            val result = mutableListOf<Group>()
            var node = document.firstChild
            while (node != null) {
                val group = Document()
                var count = 0
                // Fixed groups keep already displayed boundaries stable while appending.
                while (node != null && count < 8) {
                    val next = node.next
                    group.appendChild(node)
                    node = next
                    count++
                }
                val signature = signatures.render(group)
                val cached = previous.getOrNull(result.size)
                result.add(
                    if (cached?.signature == signature) cached
                    else Group(signature, renderGroup(group, signature))
                )
            }
            previous = result
            result.flatMap { it.blocks }
        }

    /** Split rendered spans, so tables at any nesting depth scroll without widening prose. */
    private fun renderGroup(group: Document, signature: String): List<MarkdownBlock> {
        val text = markwon.render(group)
        val tables =
            text.getSpans(0, text.length, TableSpan::class.java).sortedBy { text.getSpanStart(it) }
        if (tables.isEmpty()) return listOf(MarkdownBlock(signature, text))

        val columns = mutableListOf<Int>()
        group.accept(
            object : AbstractVisitor() {
                override fun visit(block: CustomBlock) {
                    if (block is TableBlock) {
                        var cell = block.firstChild
                        while (cell != null && cell !is TableCell) cell = cell.firstChild
                        var count = 0
                        while (cell != null) {
                            count++
                            cell = cell.next
                        }
                        columns.add(count)
                    }
                    visitChildren(block)
                }
            }
        )
        val blocks = mutableListOf<MarkdownBlock>()
        fun append(from: Int, to: Int, tableColumns: Int = 0) {
            var start = from
            var end = to
            while (start < end && text[start] == '\n') start++
            while (end > start && text[end - 1] == '\n') end--
            if (start == end) return
            val part = SpannableStringBuilder(text, start, end)
            // Continued list items retain indentation without repeating a bullet or number.
            for (span in text.getSpans(start, end, LeadingMarginSpan::class.java)) {
                if (
                    text.getSpanStart(span) < start &&
                        (span is BulletListItemSpan || span is OrderedListItemSpan)
                ) {
                    part.removeSpan(span)
                    part.setSpan(
                        LeadingMarginSpan.Standard(span.getLeadingMargin(false)),
                        0,
                        minOf(end, text.getSpanEnd(span)) - start,
                        Spanned.SPAN_EXCLUSIVE_EXCLUSIVE,
                    )
                }
            }
            blocks.add(MarkdownBlock("$signature#${blocks.size}", part, tableColumns))
        }
        var start = 0
        tables.forEachIndexed { index, table ->
            val tableStart = text.getSpanStart(table)
            val tableEnd = text.getSpanEnd(table)
            append(start, tableStart)
            append(tableStart, tableEnd, columns[index])
            start = tableEnd
        }
        append(start, text.length)
        return blocks
    }
}

/** Binding an unchanged group must not call TextView.setText or request a new text layout. */
internal class MarkdownTextView(context: Context, copyText: (() -> String)? = null) :
    TextView(context) {
    private var bound: MarkdownBlock? = null

    init {
        textSize = 16f
        setLineSpacing(0f, 1.2f)
        includeFontPadding = false
        setTextIsSelectable(true)
        // Selectable text installs ArrowKeyMovementMethod, so Markwon's default
        // (only installed when movementMethod is null) never handles link taps.
        movementMethod =
            object : LinkMovementMethod() {
                override fun onTouchEvent(
                    widget: TextView,
                    buffer: Spannable,
                    event: MotionEvent,
                ): Boolean =
                    TableAwareMovementMethod.handleTableRowTouchEvent(widget, buffer, event) ||
                        super.onTouchEvent(widget, buffer, event)
            }
        if (copyText != null)
            customSelectionActionModeCallback =
                object : ActionMode.Callback {
                    private val copyAll = View.generateViewId()

                    override fun onCreateActionMode(mode: ActionMode, menu: Menu): Boolean {
                        menu.add(Menu.NONE, copyAll, Menu.NONE, "Copier tout le texte")
                        return true
                    }

                    override fun onPrepareActionMode(mode: ActionMode, menu: Menu) = false

                    override fun onActionItemClicked(mode: ActionMode, item: MenuItem): Boolean {
                        if (item.itemId != copyAll) return false
                        context
                            .getSystemService(ClipboardManager::class.java)
                            .setPrimaryClip(ClipData.newPlainText("Cairn", copyText()))
                        mode.finish()
                        return true
                    }

                    override fun onDestroyActionMode(mode: ActionMode) = Unit
                }
    }

    fun bind(markwon: Markwon, block: MarkdownBlock) {
        if (bound === block) return
        markwon.setParsedMarkdown(this, block.text)
        bound = block
    }
}

/** Initial async measurement must finish before restoring a saved pixel offset. */
internal class MarkdownRendering {
    var pending by mutableIntStateOf(0)
        private set

    var revision by mutableIntStateOf(0)
        private set

    fun changed() {
        revision++
    }

    fun begin() {
        pending++
    }

    fun end() {
        pending--
    }

    suspend fun awaitLayout() {
        withFrameNanos {}
        snapshotFlow { pending }.first { it == 0 }
        withFrameNanos {}
    }
}

internal val LocalMarkdownRendering = compositionLocalOf<MarkdownRendering?> { null }

/** A render and its disposal can both finish the same initial layout. */
internal class MarkdownRenderPass(private val rendering: MarkdownRendering?) {
    private var pending = false

    fun begin() {
        if (!pending) {
            pending = true
            rendering?.begin()
        }
    }

    fun finish() {
        if (pending) {
            pending = false
            rendering?.end()
        }
    }
}
