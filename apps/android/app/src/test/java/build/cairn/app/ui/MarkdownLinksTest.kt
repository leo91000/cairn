package build.cairn.app.ui

import android.app.Activity
import android.graphics.Bitmap
import android.graphics.Canvas
import android.os.SystemClock
import android.text.Spanned
import android.text.style.ClickableSpan
import android.view.MotionEvent
import android.view.View
import io.noties.markwon.AbstractMarkwonPlugin
import io.noties.markwon.Markwon
import io.noties.markwon.MarkwonConfiguration
import io.noties.markwon.ext.tables.TablePlugin
import io.noties.markwon.ext.tables.TableRowSpan
import io.noties.markwon.ext.tables.TableTheme
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class MarkdownLinksTest {
    @Test fun `first tap opens a link inside a table cell`() = tableLink("")

    @Test fun `first tap opens a short link inside a quoted table`() = tableLink("> ")

    @Test fun `first tap opens a short link inside a listed table`() = tableLink("    ")

    private fun tableLink(nesting: String) {
        Robolectric.buildActivity(Activity::class.java).setup().use { controller ->
            val activity = controller.get()
            val opened = mutableListOf<String>()
            val markwon =
                Markwon.builder(activity)
                    .usePlugin(TablePlugin.create(activity))
                    .usePlugin(
                        object : AbstractMarkwonPlugin() {
                            override fun configureConfiguration(
                                builder: MarkwonConfiguration.Builder
                            ) {
                                builder.linkResolver { _, link -> opened.add(link) }
                            }
                        }
                    )
                    .build()
            val view = MarkdownTextView(activity)
            activity.setContentView(view)
            view.bind(
                markwon,
                MarkdownBlocks(markwon)
                    .render(
                        (if (nesting == "    ") "1. List item\n\n" else "") +
                            "| Name | Reference |\n| --- | --- |\n| One | prefix [X](https://example.com) suffix more plain words |"
                                .lineSequence()
                                .joinToString("\n") { nesting + it }
                    )
                    .single { it.tableColumns > 0 },
            )
            val bitmap = Bitmap.createBitmap(800, 500, Bitmap.Config.ARGB_8888)
            repeat(3) {
                view.measure(
                    View.MeasureSpec.makeMeasureSpec(800, View.MeasureSpec.EXACTLY),
                    View.MeasureSpec.makeMeasureSpec(500, View.MeasureSpec.EXACTLY),
                )
                view.layout(0, 0, 800, 500)
                view.draw(Canvas(bitmap))
                org.robolectric.shadows.ShadowLooper.idleMainLooper()
            }
            val text = view.text as Spanned
            val row =
                text.getSpans(0, text.length, TableRowSpan::class.java).maxBy {
                    text.getSpanStart(it)
                }
            val line = view.layout.getLineForOffset(text.getSpanStart(row))
            val cell = row.findLayoutForHorizontalOffset(row.cellWidth() + 1)!!
            val content = cell.text as Spanned
            val link = content.getSpans(0, content.length, ClickableSpan::class.java).single()
            val padding = TableTheme.create(activity).tableCellPadding()
            val rowHeight = maxOf(row.findLayoutForHorizontalOffset(1)!!.height, cell.height)
            val extra =
                (view.layout.getLineBottom(line) - view.layout.getLineTop(line) - rowHeight) / 4
            val x =
                view.layout.getPrimaryHorizontal(text.getSpanStart(row)) +
                    row.cellWidth() +
                    padding +
                    (cell.getPrimaryHorizontal(content.getSpanStart(link)) +
                        cell.getPrimaryHorizontal(content.getSpanEnd(link))) / 2f
            val y = view.layout.getLineTop(line) + padding + extra + cell.getLineBottom(0) / 2f
            val time = SystemClock.uptimeMillis()
            for ((action, delay) in
                listOf(MotionEvent.ACTION_DOWN to 0, MotionEvent.ACTION_UP to 50)) {
                MotionEvent.obtain(time, time + delay, action, x, y, 0).let { event ->
                    view.dispatchTouchEvent(event)
                    event.recycle()
                }
            }
            assertEquals(
                "Cell=${cell.text}, cellWidth=${row.cellWidth()}, line=$line, x=$x, y=$y, offset=${view.layout.getOffsetForHorizontal(line, x)}, span=${text.getSpanStart(row)}..${text.getSpanEnd(row)}",
                listOf("https://example.com"),
                opened,
            )
            bitmap.recycle()
        }
    }

    @Test
    fun `first tap opens a markdown link in selectable text exactly once`() {
        Robolectric.buildActivity(Activity::class.java).setup().use { controller ->
            val activity = controller.get()
            val opened = mutableListOf<String>()
            val markwon =
                Markwon.builder(activity)
                    .usePlugin(
                        object : AbstractMarkwonPlugin() {
                            override fun configureConfiguration(
                                builder: MarkwonConfiguration.Builder
                            ) {
                                builder.linkResolver { _, link -> opened.add(link) }
                            }
                        }
                    )
                    .build()
            val view = MarkdownTextView(activity)
            activity.setContentView(view)
            view.bind(
                markwon,
                MarkdownBlocks(markwon)
                    .render("Texte [Documentation](https://example.com) suite")
                    .single(),
            )
            view.measure(
                View.MeasureSpec.makeMeasureSpec(800, View.MeasureSpec.EXACTLY),
                View.MeasureSpec.makeMeasureSpec(500, View.MeasureSpec.EXACTLY),
            )
            view.layout(0, 0, 800, 500)
            val text = view.text as Spanned
            val span = text.getSpans(0, text.length, ClickableSpan::class.java).single()
            val start = text.getSpanStart(span)
            val x = view.layout.getPrimaryHorizontal(start + 2)
            val y = view.layout.getLineBottom(0) / 2f
            val time = SystemClock.uptimeMillis()
            for ((action, delay) in
                listOf(MotionEvent.ACTION_DOWN to 0, MotionEvent.ACTION_UP to 50)) {
                MotionEvent.obtain(time, time + delay, action, x, y, 0).let { event ->
                    view.dispatchTouchEvent(event)
                    event.recycle()
                }
            }
            assertEquals(listOf("https://example.com"), opened)
            assertTrue(view.isTextSelectable)
        }
    }
}
