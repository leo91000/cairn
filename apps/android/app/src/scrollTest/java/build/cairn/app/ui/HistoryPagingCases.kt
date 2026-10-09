package build.cairn.app.ui

import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.*
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.ComposeContentTestRule
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.dp
import build.cairn.app.data.LiveSnapshot
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

/**
 * Inverted infinite scroll on the real lazy list, async Markdown renderer and production paging
 * hook. Pages are applied exactly as the app does: the older rows are prepended, nothing else is
 * touched.
 */
abstract class HistoryPagingCases(
    @get:Rule val compose: ComposeContentTestRule = createComposeRule()
) {
    private lateinit var list: LazyListState
    private lateinit var rendering: MarkdownRendering
    private lateinit var scope: CoroutineScope
    private var numbers by mutableStateOf((21..40).toList())
    private var loading by mutableStateOf(false)
    private var hasOlder by mutableStateOf(true)
    private var error by mutableStateOf<String?>(null)
    private var ready by mutableStateOf(false)
    private var requests = 0

    /** Answer each request immediately with [size] older rows, like a warm server. */
    private var autoPage: Int? = null
    private var initialFirst: Pair<Any, Int>? = null

    private fun start(
        index: Int = 0,
        offset: Int = 420,
        short: Boolean = false,
        expectLoading: Boolean = true,
    ) {
        compose.setContent {
            list = rememberLazyListState()
            rendering = remember { MarkdownRendering() }
            scope = rememberCoroutineScope()
            val live =
                LiveSnapshot(
                    oldest = numbers.first().toLong(),
                    hasOlder = hasOlder,
                    loadingOlder = loading,
                    olderError = error,
                    loadOlder = {
                        requests++
                        error = null
                        loading = true
                    },
                )
            val load = rememberHistoryPaging(live, list, ready)
            LaunchedEffect(loading) {
                val size = autoPage
                if (loading && size != null) {
                    val first = numbers.first()
                    numbers = (maxOf(1, first - size) until first).toList() + numbers
                    hasOlder = numbers.first() > 1
                    loading = false
                }
            }
            CairnTheme("dark") {
                Surface(Modifier.fillMaxWidth().height(620.dp)) {
                    CompositionLocalProvider(LocalMarkdownRendering provides rendering) {
                        Box {
                            LazyColumn(
                                modifier = Modifier.fillMaxSize().testTag("paged-history"),
                                state = list,
                                contentPadding = PaddingValues(16.dp),
                                verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.Bottom),
                            ) {
                                items(numbers, key = { "message:$it" }) { number ->
                                    if (short) Text("Étape $number")
                                    else
                                        Markdown(
                                            (1..35).joinToString("\n\n") {
                                                "Message $number, paragraphe $it. Le lecteur doit garder exactement le même texte devant les yeux."
                                            }
                                        )
                                }
                            }
                            HistoryStatus(live, list, load)
                        }
                    }
                }
            }
        }
        compose.waitUntil(20000) {
            rendering.pending == 0 && list.layoutInfo.visibleItemsInfo.isNotEmpty()
        }
        var positioned = false
        compose.runOnIdle {
            scope.launch {
                list.scrollToItem(index, offset)
                rendering.awaitLayout()
                list.scrollToItem(index, offset)
                positioned = true
            }
        }
        compose.waitUntil(20000) { positioned }
        compose.runOnIdle {
            initialFirst = list.layoutInfo.visibleItemsInfo.first().let { it.key to it.offset }
            ready = true
        }
        compose.waitForIdle()
        if (expectLoading) compose.waitUntil(10000) { loading || requests > 0 }
        else compose.waitForIdle()
    }

    private fun applyPage(from: Int, more: Boolean = true) {
        compose.runOnIdle {
            numbers = (from until numbers.first()).toList() + numbers
            hasOlder = more
            loading = false
        }
        compose.waitForIdle()
        compose.mainClock.advanceTimeBy(200)
        compose.waitForIdle()
    }

    private fun capture(name: String) {
        val directory = System.getProperty("cairn.screenshots.dir") ?: return
        File(directory).mkdirs()
        val bitmap = compose.onRoot().captureToImage().asAndroidBitmap()
        File(directory, "$name.png").outputStream().use {
            bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
        }
    }

    private fun offset(number: Int = 21) =
        list.layoutInfo.visibleItemsInfo.firstOrNull { it.key == "message:$number" }?.offset

    @Test
    fun olderPageKeepsTheSameTextAtTheSamePixel() {
        start()
        val before = compose.runOnIdle { offset()!! }
        capture("before-page")
        applyPage(16, more = false)
        compose.runOnIdle {
            assertEquals("The older page must be attached", 25, list.layoutInfo.totalItemsCount)
            assertEquals(
                "Prepending must preserve the visible paragraph, not jump to a different message",
                before,
                offset(),
            )
            assertEquals(1, requests)
        }
        capture("after-page")
    }

    @Test
    fun pageArrivingAtTheVeryStartKeepsTheFirstParagraph() {
        start(offset = 0)
        compose.runOnIdle {
            assertFalse("The reader is at the start of the loaded history", list.canScrollBackward)
        }
        compose.onNodeWithText("Chargement des messages précédents…").assertIsDisplayed()
        val before = compose.runOnIdle { offset()!! }
        applyPage(16, more = false)
        compose.runOnIdle {
            assertEquals(
                "The first loaded paragraph must not be replaced by the new page",
                before,
                offset(),
            )
            assertTrue("The new page is above the reader", list.canScrollBackward)
        }
        compose.onAllNodesWithText("Chargement des messages précédents…").assertCountEquals(0)
    }

    @Test
    fun scrollingWhilePageLoadsKeepsTheLatestReadingPosition() {
        start()
        var moved = false
        compose.runOnIdle {
            scope.launch {
                list.scrollBy(-170f)
                moved = true
            }
        }
        compose.waitUntil(10000) { moved }
        val before = compose.runOnIdle { offset()!! }
        applyPage(16, more = false)
        compose.runOnIdle {
            assertEquals(
                "Loading must not undo scrolling performed while waiting",
                before,
                offset(),
            )
        }
    }

    @Test
    fun pageArrivingDuringAHeldDragKeepsTheTextAndTheGesture() {
        start(offset = 100)
        val history = compose.onNodeWithTag("paged-history")
        history.performTouchInput {
            down(Offset(2f, 180f))
            moveBy(Offset(0f, 60f), delayMillis = 160)
        }
        compose.runOnIdle {
            assertTrue("The finger must still be dragging", list.isScrollInProgress)
        }
        val before = compose.runOnIdle { offset()!! }
        applyPage(16)
        compose.runOnIdle {
            assertTrue("The page must not cancel the finger's scroll", list.isScrollInProgress)
            assertEquals(
                "New history must not replace the text under the held finger",
                before,
                offset(),
            )
        }
        history.performTouchInput { moveBy(Offset(0f, 48f), delayMillis = 160) }
        // Device input is dispatched asynchronously; await the MOVE's resulting layout.
        try {
            compose.waitUntil(5000) { (offset() ?: Int.MIN_VALUE) > before }
        } catch (failure: Throwable) {
            throw AssertionError(
                "The held MOVE was not applied: before=$before, after=${offset()}, " +
                    "scrolling=${list.isScrollInProgress}, visible=${list.layoutInfo.visibleItemsInfo.map { it.key to it.offset }}",
                failure,
            )
        }
        history.performTouchInput {
            advanceEventTime(200)
            up()
        }
        compose.waitForIdle()
    }

    @Test
    fun shortRowsKeepLoadingUntilSeveralScreensAreBuffered() {
        numbers = (971..1000).toList()
        autoPage = 10
        start(index = 29, offset = 0, short = true)
        compose.waitUntil(20000) {
            !loading &&
                list.distanceToStart() >=
                    HISTORY_PREFETCH_SCREENS * list.layoutInfo.viewportSize.height
        }
        compose.mainClock.advanceTimeBy(500)
        compose.waitForIdle()
        compose.runOnIdle {
            assertTrue("Folded pages that add little text must chain, got $requests", requests > 1)
            assertTrue(
                "Loading stops once enough history is buffered",
                hasOlder && numbers.size < 500,
            )
            val (key, offset) = initialFirst!!
            assertEquals(
                "The reader stays on the same row",
                offset,
                list.layoutInfo.visibleItemsInfo.first { it.key == key }.offset,
            )
        }
        val settled = compose.runOnIdle { requests }
        compose.mainClock.advanceTimeBy(1000)
        compose.runOnIdle {
            assertEquals("No request while the reader does not move", settled, requests)
        }
    }

    @Test
    fun shortHistoryThatDoesNotFillTheScreenStaysInPlace() {
        numbers = (991..1000).toList()
        autoPage = 10
        start(offset = 0, short = true)
        compose.waitUntil(20000) { !loading && requests > 1 }
        compose.mainClock.advanceTimeBy(500)
        compose.waitForIdle()
        compose.runOnIdle {
            val (key, offset) = initialFirst!!
            assertEquals(
                "Older rows appear above a short history without moving it",
                offset,
                list.layoutInfo.visibleItemsInfo.first { it.key == key }.offset,
            )
        }
    }

    @Test
    fun readingFarFromTheStartDoesNotLoad() {
        start(index = 12, offset = 0, expectLoading = false)
        compose.mainClock.advanceTimeBy(300)
        compose.runOnIdle {
            assertEquals("Twelve long messages above the reader are enough", 0, requests)
        }
    }

    @Test
    fun failedPageStopsAutomaticLoadingUntilRetry() {
        start()
        compose.runOnIdle {
            loading = false
            error = "Historique indisponible. Réessayez."
        }
        compose.mainClock.advanceTimeBy(500)
        compose.waitForIdle()
        compose.runOnIdle { assertEquals("A failure must not loop", 1, requests) }
        compose.onNodeWithText("Réessayer").performClick()
        compose.runOnIdle {
            assertEquals(2, requests)
            assertTrue(loading)
        }
    }

    @Test
    fun consecutivePagesKeepTheCurrentParagraphWithoutDuplicateRequests() {
        start()
        val before = compose.runOnIdle { offset()!! }
        applyPage(16)
        compose.runOnIdle {
            assertEquals(before, offset())
            assertEquals(1, requests)
        }
        // Five long messages are buffered above: reading on up approaches the start again.
        var moved = false
        compose.runOnIdle {
            scope.launch {
                list.scrollToItem(0, 300)
                rendering.awaitLayout()
                list.scrollToItem(0, 300)
                moved = true
            }
        }
        compose.waitUntil(20000) { moved }
        compose.waitUntil(10000) { loading }
        val nextBefore = compose.runOnIdle { offset(16)!! }
        applyPage(11, more = false)
        compose.runOnIdle {
            assertEquals(nextBefore, offset(16))
            assertEquals("Only one request per page", 2, requests)
        }
    }

    @Test
    fun returningToTheLatestMessageWhileLoadingIsNotUndone() {
        start()
        var done = false
        compose.runOnIdle {
            scope.launch {
                list.scrollToItem(19, 300)
                rendering.awaitLayout()
                list.scrollToItem(19, 300)
                done = true
            }
        }
        compose.waitUntil(20000) { done }
        compose.waitForIdle()
        val before = compose.runOnIdle { offset(40)!! }
        applyPage(16, more = false)
        compose.runOnIdle { assertEquals(before, offset(40)) }
    }
}
