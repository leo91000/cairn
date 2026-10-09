package build.cairn.app.ui

import org.junit.Assert.assertEquals
import org.junit.Test

@org.junit.runner.RunWith(org.robolectric.RobolectricTestRunner::class)
@org.robolectric.annotation.Config(sdk = [36])
class MarkdownRenderPassTest {
    @Test
    fun completionAndDisposalOnlyFinishEachRenderOnce() {
        val rendering = MarkdownRendering()
        val first = MarkdownRenderPass(rendering)
        val second = MarkdownRenderPass(rendering)
        first.begin()
        second.begin()
        assertEquals(2, rendering.pending)
        first.finish()
        first.finish()
        assertEquals(1, rendering.pending)
        second.finish()
        second.finish()
        assertEquals(0, rendering.pending)
    }

    @Test
    fun aReactivatedReaderStartsANewPendingLayout() {
        val rendering = MarkdownRendering()
        val pass = MarkdownRenderPass(rendering)
        pass.begin()
        pass.finish()
        pass.begin()
        assertEquals(1, rendering.pending)
        pass.finish()
        assertEquals(0, rendering.pending)
    }
}
