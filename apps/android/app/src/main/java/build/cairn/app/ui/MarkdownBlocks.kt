package build.cairn.app.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.text.Spannable
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.method.LinkMovementMethod
import android.text.style.ClickableSpan
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
import io.noties.markwon.core.spans.CodeBlockSpan
import io.noties.markwon.core.spans.OrderedListItemSpan
import io.noties.markwon.ext.tables.TableRowSpan
import io.noties.markwon.ext.tables.TableSpan
import io.noties.markwon.ext.tables.TableTheme
import kotlinx.coroutines.flow.first
import org.commonmark.ext.gfm.strikethrough.StrikethroughExtension
import org.commonmark.ext.gfm.tables.TableBlock
import org.commonmark.ext.gfm.tables.TableCell
import org.commonmark.ext.gfm.tables.TablesExtension
import org.commonmark.node.AbstractVisitor
import org.commonmark.node.BlockQuote
import org.commonmark.node.CustomBlock
import org.commonmark.node.Document
import org.commonmark.node.FencedCodeBlock
import org.commonmark.node.IndentedCodeBlock
import org.commonmark.node.ListBlock
import org.commonmark.node.ListItem
import org.commonmark.node.Node
import org.commonmark.renderer.html.HtmlRenderer

/** A rendered group is immutable and shared with the UI until its parsed content changes. */
internal class MarkdownBlock(
    val signature: String,
    val text: Spanned,
    val tableColumns: Int = 0,
    /** A closed Mermaid fence, drawn natively instead of its code. */
    val diagram: DiagramRender? = null,
)

/**
 * Parse the full document so references, unfinished fences and lists stay correct. Render small
 * groups of complete top-level nodes, retaining unchanged groups. HTML is only a canonical AST
 * fingerprint (never displayed or loaded in a WebView). Each instance belongs to one sequential
 * background collector.
 */
internal class MarkdownBlocks(
    private val markwon: Markwon,
    measure: TextMeasure = TextMeasure { text, size, _ -> text.length * size * 0.56 },
) {
    private val signatures =
        HtmlRenderer.builder()
            .extensions(listOf(TablesExtension.create(), StrikethroughExtension.create()))
            .build()
    private val diagrams = DiagramCache(measure)

    private class Group(val signature: String, val blocks: List<MarkdownBlock>)

    private var previous = emptyList<Group>()

    fun render(content: String): List<MarkdownBlock> =
        Performance.measure(PerfOperation.MARKDOWN) {
            val document = markwon.parse(content)
            // A Mermaid fence still streaming stays code until its closing fence arrives, so
            // drawings never flicker. Its group's signature changes once the fence closes.
            val streaming = streamingFence(document, content)
            var streamingTop: Node? = streaming
            while (streamingTop != null && streamingTop.parent !== document) streamingTop =
                streamingTop.parent
            val result = mutableListOf<Group>()
            var node = document.firstChild
            while (node != null) {
                val group = Document()
                var count = 0
                var open = false
                // Fixed groups keep already displayed boundaries stable while appending.
                while (node != null && count < 8) {
                    val next = node.next
                    open = open || node === streamingTop
                    group.appendChild(node)
                    node = next
                    count++
                }
                val signature = signatures.render(group) + if (open) "\u0000streaming" else ""
                val cached = previous.getOrNull(result.size)
                result.add(
                    if (cached?.signature == signature) cached
                    else Group(signature, renderGroup(group, signature, streaming))
                )
            }
            previous = result
            result.flatMap { it.blocks }
        }

    /**
     * The document's trailing Mermaid fence while its closing fence has not arrived. Only the last
     * block can still be streaming; earlier fences were closed by later content.
     */
    private fun streamingFence(document: Node, content: String): FencedCodeBlock? {
        var node = document.lastChild
        while (node is BlockQuote || node is ListBlock || node is ListItem) node = node.lastChild
        val fence = node as? FencedCodeBlock ?: return null
        if (!isDiagramLanguage(fenceLanguage(fence.info))) return null
        // A real closing delimiter is present in raw Markdown but excluded from its parsed literal.
        fun trailingFences(text: String): Int {
            var count = 0
            var end = text.length
            while (end > 0) {
                val start = text.lastIndexOf('\n', end - 1)
                val line = text.substring(start + 1, end).trimStart(' ', '\t', '>').trim()
                end = start
                if (line.isEmpty()) continue
                if (line.length < fence.fenceLength || line.any { it != fence.fenceChar }) break
                count++
            }
            return count
        }
        // Literal lines may themselves end with indented ticks; count the entire trailing run.
        val closed = trailingFences(content) > trailingFences(fence.literal)
        return fence.takeUnless { closed }
    }

    /**
     * Split rendered spans, so tables at any nesting depth scroll without widening prose and closed
     * Mermaid fences become drawings between the text around them.
     */
    private fun renderGroup(
        group: Document,
        signature: String,
        streaming: FencedCodeBlock?,
    ): List<MarkdownBlock> {
        val text = markwon.render(group)
        val tables =
            text.getSpans(0, text.length, TableSpan::class.java).sortedBy { text.getSpanStart(it) }
        val codes =
            text.getSpans(0, text.length, CodeBlockSpan::class.java).sortedBy {
                text.getSpanStart(it)
            }
        if (tables.isEmpty() && codes.isEmpty()) return listOf(MarkdownBlock(signature, text))

        val columns = mutableListOf<Int>()
        // One entry per code block, in document order like their spans.
        val fences = mutableListOf<DiagramRender?>()
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

                override fun visit(block: FencedCodeBlock) {
                    fences.add(
                        if (block === streaming || !isDiagramLanguage(fenceLanguage(block.info)))
                            null
                        else diagrams.render(block.literal.orEmpty().removeSuffix("\n"))
                    )
                }

                override fun visit(block: IndentedCodeBlock) {
                    fences.add(null)
                }
            }
        )
        val drawings =
            if (fences.size == codes.size)
                codes.zip(fences).mapNotNull { (span, diagram) -> diagram?.let { span to it } }
            else emptyList()
        if (tables.isEmpty() && drawings.isEmpty()) return listOf(MarkdownBlock(signature, text))

        val blocks = mutableListOf<MarkdownBlock>()
        fun append(from: Int, to: Int, tableColumns: Int = 0, diagram: DiagramRender? = null) {
            var start = from
            var end = to
            while (start < end && text[start] == '\n') start++
            while (end > start && text[end - 1] == '\n') end--
            if (start == end && diagram == null) return
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
            blocks.add(MarkdownBlock("$signature#${blocks.size}", part, tableColumns, diagram))
        }
        class Part(val start: Int, val end: Int, val columns: Int, val diagram: DiagramRender?)
        val parts =
            (tables.mapIndexed { index, table ->
                    Part(text.getSpanStart(table), text.getSpanEnd(table), columns[index], null)
                } +
                    drawings.map { (span, diagram) ->
                        Part(text.getSpanStart(span), text.getSpanEnd(span), 0, diagram)
                    })
                .sortedBy { it.start }
        var start = 0
        for (part in parts) {
            append(start, part.start)
            append(part.start, part.end, part.columns, part.diagram)
            start = part.end
        }
        append(start, text.length)
        return blocks
    }
}

/** Binding an unchanged group must not call TextView.setText or request a new text layout. */
internal class MarkdownTextView(context: Context, copyText: (() -> String)? = null) :
    TextView(context) {
    private var bound: MarkdownBlock? = null
    /** Asks a side question about the selected passage, while the agent works on a reply. */
    var askAbout: ((String) -> Unit)? = null

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
                    tableLink(widget, buffer, event) || super.onTouchEvent(widget, buffer, event)
            }
        if (copyText != null)
            customSelectionActionModeCallback =
                object : ActionMode.Callback {
                    private val copyAll = View.generateViewId()
                    private val askSelection = View.generateViewId()

                    override fun onCreateActionMode(mode: ActionMode, menu: Menu): Boolean {
                        if (askAbout != null)
                            menu.add(Menu.NONE, askSelection, Menu.NONE, "Demander à côté")
                        menu.add(Menu.NONE, copyAll, Menu.NONE, "Copier tout le texte")
                        return true
                    }

                    override fun onPrepareActionMode(mode: ActionMode, menu: Menu) = false

                    override fun onActionItemClicked(mode: ActionMode, item: MenuItem): Boolean {
                        if (item.itemId == askSelection) {
                            val start = minOf(selectionStart, selectionEnd).coerceAtLeast(0)
                            val end = maxOf(selectionStart, selectionEnd).coerceAtMost(text.length)
                            val quote = text.subSequence(start, end).toString().trim()
                            mode.finish()
                            if (quote.isNotEmpty()) askAbout?.invoke(quote)
                            return true
                        }
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

    /** Markwon's table hit test omits the leading margin and the cell's drawing offsets. */
    private fun tableLink(widget: TextView, buffer: Spannable, event: MotionEvent): Boolean {
        if (event.action != MotionEvent.ACTION_UP) return false
        val layout = widget.layout ?: return false
        val y = event.y - widget.totalPaddingTop + widget.scrollY
        if (y < 0 || y >= layout.height) return false
        val line = layout.getLineForVertical(y.toInt())
        val row =
            buffer
                .getSpans(
                    layout.getLineStart(line),
                    layout.getLineEnd(line),
                    TableRowSpan::class.java,
                )
                .firstOrNull() ?: return false
        // Cell layouts are created on the first draw, which may still be pending.
        val width = runCatching { row.cellWidth() }.getOrDefault(0)
        if (width <= 0) return false
        val x =
            event.x - widget.totalPaddingLeft + widget.scrollX -
                layout.getPrimaryHorizontal(buffer.getSpanStart(row))
        if (x < 0) return false
        val cell = row.findLayoutForHorizontalOffset(x.toInt()) ?: return false
        val padding = TableTheme.create(context).tableCellPadding()
        val rowHeight =
            (0 until (bound?.tableColumns ?: 0)).maxOfOrNull {
                row.findLayoutForHorizontalOffset(it * width)!!.height
            } ?: return false
        // TableRowSpan draws each cell after this additional quarter of the row's spare height.
        val spare = (layout.getLineBottom(line) - layout.getLineTop(line) - rowHeight) / 4
        val cellY = y - layout.getLineTop(line) - padding - spare
        val cellX = x % width - padding
        if (cellY < 0 || cellY >= cell.height) return false
        val cellLine = cell.getLineForVertical(cellY.toInt())
        if (cellX < cell.getLineLeft(cellLine) || cellX > cell.getLineRight(cellLine)) return false
        val offset = cell.getOffsetForHorizontal(cellLine, cellX)
        val text = cell.text as? Spanned ?: return false
        val link =
            text.getSpans(offset, offset, ClickableSpan::class.java).firstOrNull() ?: return false
        link.onClick(widget)
        return true
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
