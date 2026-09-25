package dev.leo.manager.ui

import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi", shadows = [SelectionMagnifierShadow::class])
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class HistoryFollowTest : HistoryFollowCases()

/**
 * Robolectric has no window surface for Android 16's text selection magnifier. Only the magnifier's
 * rendering is stubbed; selection and link gestures remain native. The same cases also run
 * unmodified on the CI emulator.
 */
@org.robolectric.annotation.Implements(android.widget.Magnifier::class)
class SelectionMagnifierShadow {
    @org.robolectric.annotation.Implementation fun show(x: Float, y: Float) = Unit

    @org.robolectric.annotation.Implementation
    fun show(x: Float, y: Float, mx: Float, my: Float) = Unit

    @org.robolectric.annotation.Implementation fun dismiss() = Unit

    @org.robolectric.annotation.Implementation fun update() = Unit

    @org.robolectric.annotation.Implementation fun getPosition(): android.graphics.Point? = null

    @org.robolectric.annotation.Implementation
    fun getSourcePosition(): android.graphics.Point? = null
}
