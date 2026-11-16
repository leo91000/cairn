package build.cairn.app.ui

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.junit4.ComposeContentTestRule
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelStore

/** Cancels the complete test composition before its view model and HTTP fixture are released. */
internal class FixtureUi(private val compose: ComposeContentTestRule) : AutoCloseable {
    private val models = ViewModelStore()
    private var mounted by mutableStateOf(true)
    private var hasContent = false
    private var closed = false

    fun setContent(model: ViewModel, content: @Composable () -> Unit) {
        check(!hasContent && !closed) { "A fixture hosts one composition." }
        hasContent = true
        models.put("fixture", model)
        compose.setContent { if (mounted) content() }
    }

    override fun close() {
        if (closed) return
        closed = true
        if (!hasContent) return

        val autoAdvance = compose.mainClock.autoAdvance
        try {
            compose.runOnUiThread { mounted = false }
            compose.mainClock.autoAdvance = true
            compose.waitForIdle()
        } finally {
            compose.mainClock.autoAdvance = autoAdvance
            compose.runOnUiThread { models.clear() }
        }
    }
}
