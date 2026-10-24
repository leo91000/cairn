package build.cairn.app.ui

import android.app.Activity
import android.text.Selection
import android.text.Spannable
import android.view.ActionMode
import android.view.Menu
import android.view.MenuInflater
import android.view.View
import android.widget.PopupMenu
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class MarkdownSideQuestionTest {
    private class Mode : ActionMode() {
        var finished = false

        override fun setTitle(title: CharSequence?) = Unit

        override fun setTitle(resId: Int) = Unit

        override fun setSubtitle(subtitle: CharSequence?) = Unit

        override fun setSubtitle(resId: Int) = Unit

        override fun setCustomView(view: View?) = Unit

        override fun invalidate() = Unit

        override fun finish() {
            finished = true
        }

        override fun getMenu(): Menu? = null

        override fun getTitle(): CharSequence? = null

        override fun getSubtitle(): CharSequence? = null

        override fun getCustomView(): View? = null

        override fun getMenuInflater(): MenuInflater? = null
    }

    private fun menu(view: MarkdownTextView, activity: Activity): Menu {
        val menu = PopupMenu(activity, view).menu
        view.customSelectionActionModeCallback.onCreateActionMode(Mode(), menu)
        return menu
    }

    @Test
    fun `the selection menu asks about the selected passage only while the agent can answer`() {
        Robolectric.buildActivity(Activity::class.java).setup().use { controller ->
            val activity = controller.get()
            val view = MarkdownTextView(activity) { "Tout le texte" }
            activity.setContentView(view)
            view.setText(
                "Migrer la base avant le code.",
                android.widget.TextView.BufferType.SPANNABLE,
            )
            Selection.setSelection(view.text as Spannable, 0, 15)

            val titles = { menu: Menu ->
                (0 until menu.size()).map { menu.getItem(it).title.toString() }
            }
            assertEquals(listOf("Copier tout le texte"), titles(menu(view, activity)))

            val asked = mutableListOf<String>()
            view.askAbout = { asked += it }
            val menu = menu(view, activity)
            assertEquals(listOf("Demander à côté", "Copier tout le texte"), titles(menu))
            val mode = Mode()
            view.customSelectionActionModeCallback.onActionItemClicked(mode, menu.getItem(0))
            assertEquals(listOf("Migrer la base"), asked)
            assertTrue(mode.finished)
        }
    }
}
