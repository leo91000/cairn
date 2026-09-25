package dev.leo.manager.ui

import android.app.Activity
import android.os.SystemClock
import android.text.Spanned
import android.text.style.ClickableSpan
import android.view.MotionEvent
import android.view.View
import io.noties.markwon.AbstractMarkwonPlugin
import io.noties.markwon.Markwon
import io.noties.markwon.MarkwonConfiguration
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
