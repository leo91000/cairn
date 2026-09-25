package dev.leo.manager.ui

import android.view.View
import android.view.ViewGroup
import android.view.inspector.WindowInspector
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.dp
import java.io.File
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class MarkdownStreamingTest {
    @get:Rule val compose = createComposeRule()

    private fun views(): List<MarkdownTextView> {
        fun collect(view: View): List<MarkdownTextView> =
            when (view) {
                is MarkdownTextView -> listOf(view)
                is ViewGroup -> (0 until view.childCount).flatMap { collect(view.getChildAt(it)) }
                else -> emptyList()
            }
        return WindowInspector.getGlobalWindowViews().flatMap(::collect)
    }

    @Test
    fun `saved offset is restored after asynchronous initial measurement`() {
        var restored by mutableStateOf(false)
        var savedList: androidx.compose.foundation.lazy.LazyListState? = null
        val source = (1..80).joinToString("\n\n") { "Paragraphe **$it** : texte enregistré." }
        compose.setContent {
            val list = rememberLazyListState()
            val rendering = remember { MarkdownRendering() }
            SideEffect { savedList = list }
            LaunchedEffect(Unit) {
                restoreHistoryPosition(
                    list,
                    dev.leo.manager.data.ReadingPosition(0, 400, false),
                    rendering,
                )
                restored = true
            }
            LeoTheme("dark") {
                Surface(Modifier.fillMaxSize()) {
                    CompositionLocalProvider(LocalMarkdownRendering provides rendering) {
                        LazyColumn(state = list) { item(key = "answer") { Markdown(source) } }
                    }
                }
            }
        }
        compose.waitUntil(20000) { restored }
        compose.waitForIdle()
        assertEquals(0, savedList!!.firstVisibleItemIndex)
        assertEquals(400, savedList!!.firstVisibleItemScrollOffset)
    }

    @Test
    fun `follow mode tracks asynchronous text growth and stops when reading earlier text`() {
        var source by mutableStateOf("Première réponse.")
        var follow by mutableStateOf(true)
        var savedList: androidx.compose.foundation.lazy.LazyListState? = null
        compose.setContent {
            val list = rememberLazyListState()
            val rendering = remember { MarkdownRendering() }
            SideEffect { savedList = list }
            FollowHistoryTail(list, follow, source, rendering)
            LeoTheme("dark") {
                Surface(Modifier.fillMaxSize()) {
                    CompositionLocalProvider(LocalMarkdownRendering provides rendering) {
                        LazyColumn(state = list) { item(key = "answer") { Markdown(source) } }
                    }
                }
            }
        }
        compose.waitUntil(20000) { views().isNotEmpty() }
        compose.runOnIdle {
            source =
                (1..100).joinToString("\n\n") { "Paragraphe **$it** : une longue réponse." } +
                    "\n\nFIN"
        }
        compose.waitUntil(20000) {
            views().lastOrNull()?.text?.contains("FIN") == true &&
                savedList?.canScrollForward == false
        }
        compose.runOnIdle { follow = false }
        compose.onNode(hasScrollAction()).performScrollToIndex(0)
        compose.waitForIdle()
        val offset = savedList!!.firstVisibleItemScrollOffset
        compose.runOnIdle { source += "\n\n" + "Encore du texte.\n\n".repeat(50) + "TERMINÉ" }
        compose.waitUntil(20000) { views().lastOrNull()?.text?.contains("TERMINÉ") == true }
        compose.waitForIdle()
        assertEquals(offset, savedList!!.firstVisibleItemScrollOffset)
        assertTrue(savedList!!.canScrollForward)
    }

    @Test
    fun `streaming retains prefix views delivers final text and replaces edited documents`() {
        val prefix =
            "# Une réponse en direct\n\n" +
                (1..16).joinToString("\n\n") {
                    "Paragraphe **$it** : une réponse avec du texte et un [lien](https://example.com)."
                }
        var source by mutableStateOf(prefix + "\n\nDébut")
        compose.setContent {
            LeoTheme("dark") {
                Surface(Modifier.fillMaxSize()) {
                    Column(Modifier.verticalScroll(rememberScrollState()).padding(16.dp)) {
                        Markdown(source)
                    }
                }
            }
        }
        compose.waitUntil(20000) { views().lastOrNull()?.text?.contains("Début") == true }
        val first = views().first()
        val oldText = first.text
        repeat(20) { n -> compose.runOnIdle { source = prefix + "\n\nDébut, fragment $n" } }
        compose.waitUntil(20000) { views().lastOrNull()?.text?.contains("fragment 19") == true }
        assertSame(first, views().first())
        assertSame(oldText, views().first().text)
        System.getProperty("leo.screenshots.dir")?.let { dir ->
            File(dir).mkdirs()
            compose.onRoot().captureToImage().asAndroidBitmap().let { bitmap ->
                File(dir, "streaming-markdown.png").outputStream().use {
                    bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
                }
            }
        }
        compose.runOnIdle { source = "Une réponse corrigée et **complète**." }
        compose.waitUntil(20000) {
            views().size == 1 &&
                views().single().text.toString() == "Une réponse corrigée et complète."
        }
        assertSame(first, views().single())
        compose.runOnIdle { source = "" }
        compose.waitUntil(20000) { views().isEmpty() }
    }
}
