package dev.leo.manager.ui

import android.content.Context
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ApplicationProvider
import java.io.File
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

/** Identical finger gestures on Robolectric and an Android device, using the app's Markdown. */
abstract class HistoryFollowCases {
    @get:Rule val compose = createComposeRule()
    private lateinit var list: LazyListState
    private lateinit var rendering: MarkdownRendering
    private lateinit var gesture: HistoryFollowGesture
    private var follow by mutableStateOf(true)
    private var text by
        mutableStateOf(
            (1..30).joinToString("\n\n") {
                "Paragraphe **$it**. Une réponse en cours, qui laisse le lecteur parcourir librement son historique."
            }
        )
    private var height by mutableStateOf(620)
    private var working by mutableStateOf(true)
    private val history
        get() = compose.onNodeWithTag("history")

    @Test
    fun markdownLinksOpenOnTapButNotOnLongPressOrDrag() {
        val opened = mutableListOf<String>()
        compose.setContent {
            LeoTheme("dark") {
                CompositionLocalProvider(
                    LocalArtifactLinks provides
                        {
                            opened.add(it)
                            true
                        }
                ) {
                    Surface(Modifier.fillMaxSize().testTag("links")) {
                        Column {
                            Markdown(
                                "[Documentation](https://example.com) et du texte sélectionnable."
                            )
                        }
                    }
                }
            }
        }
        fun views(view: android.view.View): List<MarkdownTextView> =
            when (view) {
                is MarkdownTextView -> listOf(view)
                is android.view.ViewGroup ->
                    (0 until view.childCount).flatMap { views(view.getChildAt(it)) }
                else -> emptyList()
            }
        fun textView() =
            android.view.inspector.WindowInspector.getGlobalWindowViews()
                .flatMap { views(it) }
                .firstOrNull { it.text.startsWith("Documentation") }
        compose.waitUntil(20000) { textView()?.layout != null }
        val node = compose.onNodeWithTag("links")
        val origin = node.fetchSemanticsNode().positionOnScreen
        val point = compose.runOnIdle {
            val view = textView()!!
            val location = IntArray(2)
            view.getLocationOnScreen(location)
            Offset(
                location[0] + view.layout.getPrimaryHorizontal(3) - origin.x,
                location[1] + view.layout.getLineBottom(0) / 2f - origin.y,
            )
        }
        node.performTouchInput { click(point) }
        compose.runOnIdle {
            assertEquals(listOf("https://example.com"), opened)
            opened.clear()
        }
        node.performTouchInput { longClick(point, 1000) }
        compose.runOnIdle {
            assertTrue("Long press must select, not open a link", opened.isEmpty())
            assertTrue(textView()!!.hasSelection())
        }
        node.performTouchInput { swipe(point, point + Offset(0f, 180f), 300) }
        compose.runOnIdle { assertTrue("Dragging a link must not open it", opened.isEmpty()) }
    }

    private fun start() {
        compose.setContent {
            list = rememberLazyListState()
            rendering = remember { MarkdownRendering() }
            gesture = rememberHistoryFollowGesture(list, follow) { follow = it }
            FollowHistoryTail(list, follow, text, rendering, gesture)
            LeoTheme("dark") {
                Surface(Modifier.fillMaxWidth().height(height.dp)) {
                    Column {
                        Text(
                            "Conversation",
                            Modifier.padding(16.dp),
                            style = MaterialTheme.typography.titleLarge,
                        )
                        CompositionLocalProvider(LocalMarkdownRendering provides rendering) {
                            LazyColumn(
                                modifier =
                                    Modifier.weight(1f)
                                        .fillMaxWidth()
                                        .testTag("history")
                                        .historyFollowGesture(gesture),
                                state = list,
                                contentPadding = PaddingValues(16.dp),
                            ) {
                                items(8, key = { "old-$it" }) {
                                    Text(
                                        "Message précédent $it",
                                        Modifier.padding(vertical = 16.dp),
                                    )
                                }
                                item("answer") { Markdown(text) }
                                if (working)
                                    item("status") {
                                        Text("L’agent travaille…", Modifier.padding(top = 12.dp))
                                    }
                            }
                        }
                        Button(onClick = { follow = true }, modifier = Modifier.testTag("bottom")) {
                            Text(if (follow) "Suivi actif" else "Derniers messages ↓")
                        }
                    }
                }
            }
        }
        settle()
        compose.waitUntil(20000) { list.canScrollBackward && !list.canScrollForward }
    }

    private fun settle() {
        compose.waitForIdle()
        compose.waitUntil(20000) { rendering.pending == 0 }
        compose.mainClock.advanceTimeBy(200)
        compose.waitForIdle()
    }

    private fun append() {
        val revision = rendering.revision
        compose.runOnIdle { text += "\n\n" + "Nouveau texte du stream. ".repeat(18) }
        // Drive recomposition and the renderer's pacing delay before awaiting its revision.
        compose.waitForIdle()
        compose.mainClock.advanceTimeBy(200)
        compose.waitForIdle()
        try {
            compose.waitUntil(20000) { rendering.revision > revision }
        } catch (failure: Throwable) {
            throw AssertionError(
                "Markdown revision $revision -> ${rendering.revision}, pending=${rendering.pending}, position=${position()}, visible=${list.layoutInfo.visibleItemsInfo.map { it.key }}, chars=${text.length}",
                failure,
            )
        }
        settle()
    }

    private fun position() = compose.runOnIdle {
        list.firstVisibleItemIndex to list.firstVisibleItemScrollOffset
    }

    protected open fun capturePinnedImage(): android.graphics.Bitmap =
        compose.onRoot().captureToImage().asAndroidBitmap()

    @Test
    fun oversizedFinalMessageStillReachesTheBottomAfterStreamingAndResize() {
        working = false
        start()
        append()
        compose.runOnIdle { height = 480 }
        settle()
        compose.runOnIdle {
            assertTrue(follow)
            assertFalse(list.canScrollForward)
        }
        history.performTouchInput { swipeDown() }
        settle()
        compose.onNodeWithTag("bottom").performClick()
        settle()
        compose.runOnIdle { assertFalse(list.canScrollForward) }
        val directory =
            File(ApplicationProvider.getApplicationContext<Context>().filesDir, "scroll-validation")
        directory.mkdirs()
        capturePinnedImage().let { bitmap ->
            File(directory, "pinned-stream.png").outputStream().use {
                bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
            }
        }
    }

    @Test
    fun arrowAndDownwardOverscrollKeepTheActualBottomPinned() {
        start()
        history.performTouchInput { swipeDown() }
        settle()
        compose.runOnIdle { assertFalse("Reading older text must unpin", follow) }
        compose.onNodeWithTag("bottom").performClick()
        settle()
        compose.runOnIdle {
            assertFalse("Arrow must include bottom padding", list.canScrollForward)
        }
        history.performTouchInput { swipeUp() }
        settle()
        compose.runOnIdle { assertTrue("An overscroll at the end must preserve follow", follow) }
        append()
        compose.runOnIdle {
            assertTrue(follow)
            assertFalse(list.canScrollForward)
        }
    }

    @Test
    fun touchPausesStreamingAndSmallGestureTowardOlderTextUnpins() {
        start()
        history.performTouchInput { down(center) }
        compose.runOnIdle { assertTrue(gesture.touching) }
        val held = position()
        append()
        assertEquals("A finger on the screen pauses follow", held, position())
        history.performTouchInput { moveBy(Offset(0f, 64f), delayMillis = 160) }
        compose.runOnIdle {
            assertFalse("One deliberate small move must unpin during streaming", follow)
        }
        append()
        history.performTouchInput {
            advanceEventTime(200)
            up()
        }
        settle()
        val reading = position()
        append()
        compose.runOnIdle {
            assertFalse(follow)
            assertTrue(list.canScrollForward)
        }
        assertEquals("New text must not pull a reader back down", reading, position())
    }

    @Test
    fun manualReturnToBottomRepinsAndDirectionReversalUnpinsAgain() {
        start()
        // Slow drag avoids a long fling, leaving the end within one swipe.
        history.performTouchInput {
            down(center)
            moveBy(Offset(0f, 100f), delayMillis = 300)
            advanceEventTime(200)
            up()
        }
        settle()
        compose.runOnIdle { assertFalse(follow) }
        history.performTouchInput { swipeUp() }
        settle()
        compose.runOnIdle {
            assertFalse(list.canScrollForward)
            assertTrue("Returning manually to the end repins", follow)
        }
        append()
        compose.runOnIdle { assertFalse(list.canScrollForward) }
        history.performTouchInput {
            down(center)
            moveBy(Offset(0f, -80f), delayMillis = 200)
            moveBy(Offset(0f, 140f), delayMillis = 300)
            advanceEventTime(200)
            up()
        }
        settle()
        compose.runOnIdle { assertFalse("Changing one's mind within a gesture must work", follow) }
        val reading = position()
        append()
        assertEquals(reading, position())
    }

    @Test
    fun accessibilityScrollCommandsRespectReadingIntent() {
        start()
        history.performSemanticsAction(SemanticsActions.ScrollBy) { assertTrue(it(0f, -120f)) }
        settle()
        compose.runOnIdle {
            assertFalse("Accessibility scrolling toward older text must unpin", follow)
        }
        val reading = position()
        append()
        assertEquals("Streaming preserves the accessibility reader's position", reading, position())
        history.performSemanticsAction(SemanticsActions.ScrollToIndex) {
            assertTrue(it(list.layoutInfo.totalItemsCount - 1))
        }
        settle()
        compose.runOnIdle {
            assertTrue("Explicitly returning to the end resumes follow", follow)
            assertFalse(list.canScrollForward)
        }
    }

    @Test
    fun holdingWithoutMovingResumesFollowOnReleaseButNeverRepinsAReader() {
        start()
        compose.runOnIdle { assertTrue("Initial history follows", follow) }
        history.performTouchInput { down(center) }
        compose.runOnIdle { assertTrue("The stationary press is observed", gesture.touching) }
        append()
        compose.runOnIdle {
            assertTrue("Streaming under a stationary finger must retain follow intent", follow)
        }
        history.performTouchInput { up() }
        settle()
        // Native text focus can report relocation after the pointer event has
        // finished, even after follow has run. Reproduce its real displacement
        // as well as the late callback on both JVM and device runners.
        compose.runOnIdle {
            val relocated = list.dispatchRawDelta(-24f)
            assertTrue("The delayed relocation actually moves the list", relocated < 0f)
            gesture.onPostScroll(Offset(0f, -relocated), Offset.Zero, NestedScrollSource.UserInput)
        }
        settle()
        compose.runOnIdle {
            assertFalse("Release clears the active touch", gesture.touching)
            assertTrue("A stationary release must resume following", follow)
            assertFalse("Resumed follow reaches the new end", list.canScrollForward)
        }
        history.performTouchInput { swipeDown() }
        settle()
        history.performTouchInput { click(center) }
        compose.runOnIdle { height = 480 }
        append()
        compose.runOnIdle {
            assertFalse("A tap, resize or stream is not an instruction to follow", follow)
        }
    }
}
