package dev.leo.manager.ui

import android.os.SystemClock
import androidx.compose.foundation.OverscrollEffect
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.overscroll
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.drawscope.ContentDrawScope
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.node.DelegatableNode
import androidx.compose.ui.node.DrawModifierNode
import androidx.compose.ui.unit.Velocity
import androidx.compose.ui.unit.dp
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class HistoryFollowDeviceTest : HistoryFollowCases() {
    // Capture the display through Android rather than drawing Compose from the test thread.
    override fun capturePinnedImage(): android.graphics.Bitmap =
        checkNotNull(InstrumentationRegistry.getInstrumentation().uiAutomation.takeScreenshot())

    // Reuse the active Compose rule so CI's existing device selection also checks paging.
    @Test
    fun olderPagesChainUntilEnoughHistoryIsBuffered() {
        object : HistoryPagingCases(compose) {}.shortRowsKeepLoadingUntilSeveralScreensAreBuffered()
    }

    @Test
    fun olderPageKeepsTheTextUnderAnActiveFinger() {
        object : HistoryPagingCases(compose) {}
            .pageArrivingDuringAHeldDragKeepsTheTextAndTheGesture()
    }

    @Test
    fun olderPageAtTheVeryStartKeepsTheFirstParagraph() {
        object : HistoryPagingCases(compose) {}.pageArrivingAtTheVeryStartKeepsTheFirstParagraph()
    }

    // The existing CI device entry point also exercises the complete native app.
    @Test
    fun compactConversationAndKeyboard() =
        ParityDeviceCases(compose).compactChatEvidenceAndSwitcher()

    @Test
    fun taskConversationAndManagement() = ParityDeviceCases(compose).taskConversationAndManagement()

    @Test fun adaptiveTaskLayout() = ParityDeviceCases(compose).adaptiveTaskLayout()

    /**
     * Like EdgeEffect, the stretch only advances from its own draw and is plain, unobserved state.
     */
    private class StretchEffect : OverscrollEffect {
        @Volatile var stretched = true
        val draws = AtomicInteger()

        override fun applyToScroll(
            delta: Offset,
            source: NestedScrollSource,
            performScroll: (Offset) -> Offset,
        ) = performScroll(delta)

        override suspend fun applyToFling(
            velocity: Velocity,
            performFling: suspend (Velocity) -> Velocity,
        ) {
            performFling(velocity)
        }

        override val isInProgress
            get() = stretched

        override val node: DelegatableNode =
            object : Modifier.Node(), DrawModifierNode {
                override fun ContentDrawScope.draw() {
                    draws.incrementAndGet()
                    drawContent()
                }
            }
    }

    // An overscroll in progress turns every touch on the history into a drag: a stretch that
    // stopped drawing swallowed taps on files and chat swipes for as long as the agent worked.
    @Test
    fun historyStretchKeepsDrawingUntilItSettles() {
        val stretch = StretchEffect()
        compose.mainClock.autoAdvance = false
        compose.setContent { Box(Modifier.size(100.dp).overscroll(RelaxingOverscroll(stretch))) }
        fun frames(count: Int) =
            repeat(count) {
                compose.mainClock.advanceTimeByFrame()
                SystemClock.sleep(50) // let the invalidated layer reach the display
            }
        frames(2)
        assertTrue("The wrapped effect still draws its stretch", stretch.draws.get() > 0)
        val settling = stretch.draws.get()
        frames(20)
        assertTrue(
            "An unsettled stretch gets frames to relax: $settling then ${stretch.draws.get()}",
            stretch.draws.get() - settling >= 10,
        )
        stretch.stretched = false
        frames(3)
        val settled = stretch.draws.get()
        frames(20)
        assertEquals("A settled stretch no longer requests frames", settled, stretch.draws.get())
    }
}
