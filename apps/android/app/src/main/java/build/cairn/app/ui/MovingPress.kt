package build.cairn.app.ui

import android.view.ViewConfiguration
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.PressInteraction
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.AwaitPointerEventScope
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.PointerInputChange
import androidx.compose.ui.input.pointer.changedToUp
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.LayoutCoordinates
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * Lets a tap survive live content moving its control between press and release, as when a queued
 * message grows the composer under the subagent bar. Compose's click handling cancels at the first
 * move outside the control's new bounds; here the release counts when it ends over the visible
 * control where it was pressed or where it is now, as on the web.
 *
 * Apply it to the content of a clickable Surface sharing [interactions], which draws the ripple.
 * Consuming the press leaves keyboard, accessibility and focus activation to that Surface.
 */
@Composable
internal fun Modifier.pressSurvivingMoves(
    interactions: MutableInteractionSource,
    inScrollingRow: Boolean = false,
    onClick: () -> Unit,
): Modifier {
    val click by rememberUpdatedState(onClick)
    val source by rememberUpdatedState(interactions)
    val scope = rememberCoroutineScope()
    val layout = remember { arrayOfNulls<LayoutCoordinates>(1) }
    return onGloballyPositioned { layout[0] = it }
        .pointerInput(inScrollingRow) {
            awaitEachGesture {
                val down = awaitFirstDown()
                val pressedLayout = layout[0]?.takeIf { it.isAttached } ?: return@awaitEachGesture
                // The Surface keeps the taps on its minimum touch target around this content.
                val position = down.position
                if (
                    position.x !in 0f..size.width.toFloat() ||
                        position.y !in 0f..size.height.toFloat()
                )
                    return@awaitEachGesture

                down.consume()
                val pressed = pressedLayout.boundsInRoot()
                val press = PressInteraction.Press(position)
                val pressSource = source
                var shown = false
                // Like Compose's clickable, a scrolling row shows the press once it is not a
                // scroll.
                val showing = scope.launch {
                    if (inScrollingRow) delay(ViewConfiguration.getTapTimeout().toLong())
                    pressSource.emit(press)
                    shown = true
                }

                var up: PointerInputChange? = null
                try {
                    up = releaseOrCancel()
                } finally {
                    showing.cancel()
                    val released = up != null
                    scope.launch {
                        if (released) {
                            if (!shown) pressSource.emit(press)
                            pressSource.emit(PressInteraction.Release(press))
                        } else if (shown) {
                            pressSource.emit(PressInteraction.Cancel(press))
                        }
                    }
                }

                val release = up ?: return@awaitEachGesture
                val current = layout[0]?.takeIf { it.isAttached } ?: return@awaitEachGesture
                val point = current.localToRoot(release.position)
                if (point in pressed || point in current.boundsInRoot()) {
                    release.consume()
                    click()
                }
            }
        }
}

/** Compose's wait for a tap's release, without cancelling when the pointer leaves the control. */
private suspend fun AwaitPointerEventScope.releaseOrCancel(): PointerInputChange? {
    while (true) {
        val event = awaitPointerEvent()
        if (event.changes.all { it.changedToUp() }) return event.changes[0]
        if (event.changes.any { it.isConsumed }) return null

        // A scrolling ancestor consumes the move after this node sees it.
        if (awaitPointerEvent(PointerEventPass.Final).changes.any { it.isConsumed }) return null
    }
}
