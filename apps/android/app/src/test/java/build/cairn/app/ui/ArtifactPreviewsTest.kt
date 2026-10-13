package build.cairn.app.ui

import android.app.Application
import android.graphics.BitmapFactory
import android.util.Base64
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import androidx.activity.ComponentActivity
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithContentDescription
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.CairnViewModel
import build.cairn.app.data.MemoryVault
import java.io.File
import java.io.RandomAccessFile
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ArtifactPreviewsTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun delimitedFilesKeepQuotedCellsMultilineValuesAndRaggedRows() {
        assertEquals(
            TablePreview(
                listOf(
                    listOf("name", "notes"),
                    listOf("Doe, Jane", "said \"hi\""),
                    listOf("Bob", "two\nlines"),
                ),
                2,
                false,
            ),
            parseDelimited(
                "${Char(0xFEFF)}name,notes\r\n\"Doe, Jane\",\"said \"\"hi\"\"\"\r\nBob,\"two\nlines\"\n",
                ',',
            ),
        )
        assertEquals(
            TablePreview(listOf(listOf("a", "b", "c"), listOf("1", "2")), 3, false),
            parseDelimited("a\tb\tc\n1\t2", '\t'),
        )
        val large = parseDelimited((0 until 5).joinToString("\n") { "row $it" }, ',', limit = 3)
        assertEquals(listOf(listOf("row 0"), listOf("row 1"), listOf("row 2")), large.rows)
        assertTrue(large.truncated)
        assertFalse(parseDelimited("a\nb\n", ',', limit = 2).truncated)
        assertEquals(listOf(listOf("")), parseDelimited("\"\"", ',').rows)
        assertEquals(listOf(listOf("a"), listOf("")), parseDelimited("a\n\"\"", ',').rows)
        assertTrue(parseDelimited("", ',').rows.isEmpty())
    }

    @Test
    fun wideRaggedTablesHaveBoundedCellCounts() {
        val table = parseDelimited(",".repeat(4999) + "\n" + "x\n".repeat(999), ',')
        assertEquals(200, table.rows.size)
        assertEquals(50, table.columns)
        assertTrue(table.rows.all { it.size <= 50 })
        assertTrue(table.truncated)
        assertTrue(parseDelimited("a,".repeat(50), ',').truncated)
    }

    @Test
    fun animatedImagesKeepTheirOriginalBytes() {
        val bytes = byteArrayOf(71, 73, 70, 56, 57, 97, 0, -1)
        val document = imageDocument(bytes, "image/gif")
        val data = document.substringAfter("data:image/gif;base64,").substringBefore('"')
        assertArrayEquals(bytes, Base64.decode(data, Base64.DEFAULT))
    }

    @Test
    fun tablesShowEveryCell() {
        compose.setContent {
            DelimitedTable(parseDelimited("Region,Revenue\nNorth,\"1,200\"\n", ','))
        }
        compose.onNodeWithText("Region").assertExists()
        compose.onNodeWithText("1,200").assertExists()
    }

    @Test
    fun vectorImagesAndPagesRenderWithoutScriptsOrNetwork() {
        val svg = "<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>"
        val document = imageDocument(svg.toByteArray(), "image/svg+xml")
        val data = document.substringAfter("data:image/svg+xml;base64,").substringBefore('"')
        assertEquals(svg, String(Base64.decode(data, Base64.DEFAULT)))
        assertFalse("SVG markup is never parsed as HTML", document.contains("<script"))

        compose.setContent { InertWebPage("<h1>Launch ready</h1>", "launch.html") }
        compose.waitForIdle()
        val web = findWebView(compose.activity.window.decorView)!!
        assertFalse(web.settings.javaScriptEnabled)
        assertTrue(web.settings.blockNetworkLoads)
        assertFalse(web.settings.allowFileAccess)
        assertFalse(web.settings.allowContentAccess)
        assertEquals("launch.html", web.contentDescription)
        assertEquals("<h1>Launch ready</h1>", shadowOf(web).lastLoadDataWithBaseURL.data)
    }

    @Test
    fun oversizedGifsKeepTheirStaticImagePreview() {
        val bytes = javaClass.getResourceAsStream("/animated.gif")!!.use { it.readBytes() }
        // Initialize Robolectric's native decoder on the test thread before the IO preview job.
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size)!!.recycle()
        val file = File.createTempFile("large-preview", ".gif")
        file.writeBytes(bytes)
        RandomAccessFile(file, "rw").use { it.setLength(20L * 1024 * 1024 + 1) }
        val application = ApplicationProvider.getApplicationContext<Application>()
        val vm =
            ViewModelProvider(
                compose.activity,
                viewModelFactory {
                    initializer { CairnViewModel(application, MemoryVault(), beaconOrigin = "") }
                },
            )[CairnViewModel::class.java]
        try {
            compose.setContent {
                CairnTheme {
                    FilePreview(vm, "", "large.gif", "image/gif", "image", file.path) {}
                }
            }
            compose.waitUntil(15000) {
                compose
                    .onAllNodesWithContentDescription("large.gif")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithContentDescription("large.gif").assertIsDisplayed()
            compose
                .onNodeWithText("Aperçu fixe : l’animation est limitée aux GIF de 20 Mo maximum.")
                .assertIsDisplayed()
        } finally {
            file.delete()
        }
    }

    private fun findWebView(view: View): WebView? =
        view as? WebView
            ?: (view as? ViewGroup)?.let { group ->
                (0 until group.childCount).firstNotNullOfOrNull {
                    findWebView(group.getChildAt(it))
                }
            }
}
