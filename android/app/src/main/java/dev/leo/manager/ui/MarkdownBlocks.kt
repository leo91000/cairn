package dev.leo.manager.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.text.Spanned
import android.text.method.LinkMovementMethod
import android.view.ActionMode
import android.view.Menu
import android.view.MenuItem
import android.view.View
import android.widget.TextView
import androidx.compose.runtime.*
import io.noties.markwon.Markwon
import kotlinx.coroutines.flow.first
import org.commonmark.ext.gfm.strikethrough.StrikethroughExtension
import org.commonmark.ext.gfm.tables.TablesExtension
import org.commonmark.node.Document
import org.commonmark.renderer.html.HtmlRenderer

/** A rendered group is immutable and shared with the UI until its parsed content changes. */
internal class MarkdownBlock(val signature: String, val text: Spanned)

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
    private var previous = emptyList<MarkdownBlock>()

    fun render(content: String): List<MarkdownBlock> {
        val document = markwon.parse(content)
        val result = mutableListOf<MarkdownBlock>()
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
                else MarkdownBlock(signature, markwon.render(group))
            )
        }
        previous = result
        return result
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
        movementMethod = LinkMovementMethod.getInstance()
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
                            .setPrimaryClip(ClipData.newPlainText("Leo", copyText()))
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
