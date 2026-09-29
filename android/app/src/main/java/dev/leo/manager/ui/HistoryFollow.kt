package dev.leo.manager.ui

import androidx.compose.foundation.OverscrollEffect
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.rememberOverscrollEffect
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.drawscope.ContentDrawScope
import androidx.compose.ui.input.key.*
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.PointerEventType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.node.DelegatableNode
import androidx.compose.ui.node.DelegatingNode
import androidx.compose.ui.node.DrawModifierNode
import androidx.compose.ui.node.invalidateDraw
import androidx.compose.ui.semantics.scrollBy
import androidx.compose.ui.semantics.scrollByOffset
import androidx.compose.ui.semantics.scrollToIndex
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Velocity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.launch

/** Observe gestures without consuming them or deriving intent from streaming layout changes. */
internal class HistoryFollowGesture(
    private val list: LazyListState,
    private val scope: CoroutineScope,
    private val emitFollow: (Boolean) -> Unit,
) : NestedScrollConnection {
    // Stop already queued follow work immediately; recomposition can occur
    // after its next frame callback, especially for semantic scroll actions.
    var followRequested = true
        private set

    fun syncFollow(value: Boolean) {
        followRequested = value
    }

    private fun changeFollow(value: Boolean) {
        followRequested = value
        emitFollow(value)
    }

    var touching by mutableStateOf(false)
        private set

    private var flinging by mutableStateOf(false)
    private var moved = false
    private var pointerMoved = false
    private var nonTouchScroll = false
    private var semanticScrolls by mutableIntStateOf(0)
    val busy
        get() = touching || flinging || semanticScrolls > 0

    // Accessibility actions are explicit reading intent too. Native focus
    // relocation has no such action and must not silently turn following off.
    fun semanticScrollBy(y: Float) = semanticScroll { list.scrollBy(y) }

    suspend fun semanticScrollOffset(offset: Offset): Offset {
        changeFollow(false)
        semanticScrolls++
        try {
            val consumed = list.scrollBy(offset.y)
            changeFollow(!list.canScrollForward)
            return Offset(0f, consumed)
        } finally {
            semanticScrolls--
        }
    }

    fun semanticScrollTo(index: Int): Boolean {
        require(index in 0 until list.layoutInfo.totalItemsCount) {
            "Invalid history index: $index"
        }
        return semanticScroll { list.scrollToItem(index) }
    }

    private fun semanticScroll(action: suspend () -> Unit): Boolean {
        changeFollow(false)
        semanticScrolls++
        scope.launch {
            try {
                action()
                changeFollow(!list.canScrollForward)
            } finally {
                semanticScrolls--
            }
        }
        return true
    }

    fun contact(down: Boolean) {
        touching = down
        if (down) {
            flinging = false
            moved = false
            pointerMoved = false
            nonTouchScroll = false
        }
    }

    fun motion() {
        pointerMoved = true
    }

    fun wheelOrKey() {
        nonTouchScroll = true
    }

    override fun onPostScroll(
        consumed: Offset,
        available: Offset,
        source: NestedScrollSource,
    ): Offset {
        // Native selectable text can relocate after release as well as during
        // rebinding. Require an observed input, not just a UserInput scroll label.
        val directInput =
            source == NestedScrollSource.UserInput && ((touching && pointerMoved) || nonTouchScroll)
        if (directInput || flinging) {
            if (consumed.y > 0f) {
                // Touch slop has already been handled by LazyColumn. Any actual
                // movement toward older content wins, even during a fast stream.
                moved = true
                changeFollow(false)
            } else if (consumed.y < 0f || available.y < 0f) {
                moved = true
                // Only a user scroll toward the end can re-arm follow. Incoming
                // text, keyboard resizing and programmatic scrolling cannot.
                if (!list.canScrollForward) changeFollow(true)
            }
        }
        return Offset.Zero
    }

    override suspend fun onPreFling(available: Velocity): Velocity {
        flinging = moved && available.y != 0f
        return Velocity.Zero
    }

    override suspend fun onPostFling(consumed: Velocity, available: Velocity): Velocity {
        flinging = false
        return Velocity.Zero
    }
}

@Composable
internal fun rememberHistoryFollowGesture(
    list: LazyListState,
    follow: Boolean,
    changeFollow: (Boolean) -> Unit,
): HistoryFollowGesture {
    val currentChange by rememberUpdatedState(changeFollow)
    val scope = rememberCoroutineScope()
    return remember(list, scope) { HistoryFollowGesture(list, scope) { currentChange(it) } }
        .also { it.syncFollow(follow) }
}

internal fun Modifier.historyFollowGesture(gesture: HistoryFollowGesture): Modifier =
    nestedScroll(gesture)
        .semantics {
            scrollBy { _, y -> gesture.semanticScrollBy(y) }
            scrollByOffset { gesture.semanticScrollOffset(it) }
            scrollToIndex { gesture.semanticScrollTo(it) }
        }
        .onPreviewKeyEvent { event ->
            if (
                event.type == KeyEventType.KeyDown &&
                    event.key in
                        listOf(
                            Key.DirectionUp,
                            Key.DirectionDown,
                            Key.PageUp,
                            Key.PageDown,
                            Key.MoveHome,
                            Key.MoveEnd,
                            Key.Spacebar,
                        )
            )
                gesture.wheelOrKey()
            false
        }
        .pointerInput(gesture) {
            awaitPointerEventScope {
                while (true) {
                    if (awaitPointerEvent(PointerEventPass.Initial).type == PointerEventType.Scroll)
                        gesture.wheelOrKey()
                }
            }
        }
        .pointerInput(gesture) {
            awaitEachGesture {
                // Initial pass also observes touches handled by selectable Android text.
                awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
                gesture.contact(true)
                try {
                    do {
                        val event = awaitPointerEvent(PointerEventPass.Initial)
                        if (event.changes.any { it.position != it.previousPosition })
                            gesture.motion()
                    } while (event.changes.any { it.pressed })
                    // Keep the contact active until selectable child views have handled
                    // the release; their focus relocation is still part of this tap.
                    awaitPointerEvent(PointerEventPass.Final)
                } finally {
                    gesture.contact(false)
                }
            }
        }

/**
 * Android's stretch overscroll only relaxes while its node keeps drawing, and it asks for those
 * frames through a state write that, on device, stopped redrawing the history's layer: the stretch
 * froze mid-release. A scrollable with overscroll in progress starts dragging on every down, so
 * taps and horizontal swipes over the history would never reach its items or the chat pager.
 */
@Composable
internal fun rememberHistoryOverscroll(): OverscrollEffect? {
    val effect = rememberOverscrollEffect() ?: return null
    return remember(effect) { RelaxingOverscroll(effect) }
}

internal class RelaxingOverscroll(private val effect: OverscrollEffect) :
    OverscrollEffect by effect {
    override val node: DelegatableNode = RelaxingOverscrollNode(effect)
}

private class RelaxingOverscrollNode(effect: OverscrollEffect) : DelegatingNode() {
    init {
        // A delegating node that also drew itself would hide the effect's own draw node.
        delegate(RelaxDrawNode(effect))
        delegate(effect.node)
    }
}

private class RelaxDrawNode(private val effect: OverscrollEffect) :
    Modifier.Node(), DrawModifierNode {
    private var frameRequested = false

    override fun ContentDrawScope.draw() {
        drawContent()
        // Ask for the next frame from the overscroll's own layer, outside this draw pass,
        // until the effect has settled.
        if (effect.isInProgress && !frameRequested) {
            frameRequested = true
            coroutineScope.launch {
                try {
                    withFrameNanos {}
                    invalidateDraw()
                } finally {
                    frameRequested = false
                }
            }
        }
    }
}

/** Follow measured content, while allowing the user's touch or fling to take over immediately. */
@Composable
internal fun FollowHistoryTail(
    list: LazyListState,
    enabled: Boolean,
    content: Any?,
    rendering: MarkdownRendering,
    gesture: HistoryFollowGesture? = null,
) {
    val currentContent by rememberUpdatedState(content)
    val allowed = enabled && gesture?.busy != true
    LaunchedEffect(list, allowed, rendering, gesture) {
        if (allowed)
            snapshotFlow {
                val info = list.layoutInfo
                val tail = info.visibleItemsInfo.lastOrNull()
                // Selectable native text may relocate after release without changing
                // content or viewport size. Observe its measured position as well;
                // intentional gestures disable this collector before it can repin.
                val geometry =
                    listOf(
                        info.totalItemsCount,
                        info.viewportSize.height,
                        info.viewportEndOffset,
                        info.afterContentPadding,
                        tail?.index,
                        tail?.offset,
                        tail?.size,
                    )
                Triple(geometry, rendering.revision, currentContent)
            }
                .conflate()
                .collect {
                    withFrameNanos {}
                    if (gesture?.let { it.busy || !it.followRequested } == true) return@collect
                    val count = list.layoutInfo.totalItemsCount
                    if (count > 0) {
                        // Locate the last item first, then use its measured bottom. Its
                        // height may exceed the viewport, and content padding counts too.
                        if (list.layoutInfo.visibleItemsInfo.none { it.index == count - 1 }) {
                            list.scrollToItem(count - 1)
                            withFrameNanos {}
                        }
                        if (gesture?.let { it.busy || !it.followRequested } == true) return@collect
                        val info = list.layoutInfo
                        val last = info.visibleItemsInfo.lastOrNull { it.index == count - 1 }
                        if (last != null) {
                            val remaining =
                                last.offset + last.size + info.afterContentPadding -
                                    info.viewportEndOffset
                            if (remaining > 0) list.scrollBy(remaining.toFloat())
                        }
                    }
                }
    }
}
