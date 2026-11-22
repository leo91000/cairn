package build.cairn.app.ui

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
    onClick: () -> Unit,
): Modifier {
    val click by rememberUpdatedState(onClick)
    val source by rememberUpdatedState(interactions)
    val scope = rememberCoroutineScope()
    val layout = remember { arrayOfNulls<LayoutCoordinates>(1) }
    return onGloballyPositioned { layout[0] = it }
        .pointerInput(Unit) {
            awaitEachGesture {
                val down = awaitFirstDown()
                val pressedLayout = layout[0]?.takeIf { it.isAttached } ?: return@awaitEachGesture
                down.consume()
                val pressed = pressedLayout.boundsInRoot()
                val press = PressInteraction.Press(down.position)
                val pressSource = source
                scope.launch { pressSource.emit(press) }

                val up = releaseOrCancel()
                val current = layout[0]?.takeIf { it.isAttached }
                val released =
                    up != null &&
                        current != null &&
                        current.localToRoot(up.position).let {
                            it in pressed || it in current.boundsInRoot()
                        }
                scope.launch {
                    pressSource.emit(
                        if (up != null) PressInteraction.Release(press)
                        else PressInteraction.Cancel(press)
                    )
                }
                if (released && up != null) {
                    up.consume()
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
