package dev.leo.manager.ui

import android.app.Application
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Color
import androidx.compose.runtime.*
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import dev.leo.manager.data.*
import java.io.ByteArrayOutputStream
import java.io.File
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import okio.Buffer
import org.junit.*
import org.junit.Assert.*
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ArtifactSwipeTest {
    @get:Rule val compose = createComposeRule()
    private val restoration = StateRestorationTester(compose)
    private val pager
        get() = compose.onNodeWithTag("artifact-pager")

    private val files =
        (1..5).map { index ->
            Deliverable(
                id = "$index",
                runId = "fixture",
                key = "$index",
                name = "Image $index.png",
                kind = "image",
                mediaType = "image/png",
                size = 1024,
                createdAt = index.toLong(),
            )
        }
    private val downloads = CopyOnWriteArrayList<String>()

    @Before
    fun setup() {
        // The gallery decodes files on an IO thread. Robolectric's native decoder must first run on
        // the
        // test thread: initialized from a worker, it cannot resolve java.nio classes and aborts the
        // JVM.
        val warm = File.createTempFile("warm", ".png")
        warm.outputStream().use {
            Bitmap.createBitmap(1, 1, Bitmap.Config.ARGB_8888)
                .compress(Bitmap.CompressFormat.PNG, 100, it)
        }
        BitmapFactory.decodeFile(warm.path)?.recycle()
        warm.delete()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            ApplicationProvider.getApplicationContext<Application>(),
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
    }

    @After fun cleanup() = WorkManagerTestInitHelper.closeWorkDatabase()

    @Test
    fun imagesFollowTheFingerRevealNeighborsAndCanReverseBeforeRelease(): Unit = withFiles { vm ->
        ArtifactGallery(vm, files, files[0]) {}
    }
        .run {
            waitForImage(1)
            compose.waitUntil(10000) { downloads.contains("2") }
            // Opening loads this file and its immediate neighbor, not the whole gallery.
            assertEquals(setOf("1", "2"), downloads.toSet())
            val bounds = pager.getUnclippedBoundsInRoot()
            val width = bounds.right.value - bounds.left.value
            pager.performTouchInput {
                down(Offset(this.width * .9f, height * .55f))
                moveTo(Offset(this.width * .48f, height * .55f), 350)
            }
            val first = compose.onNodeWithTag("artifact-page:1").getUnclippedBoundsInRoot()
            val second = compose.onNodeWithTag("artifact-page:2").getUnclippedBoundsInRoot()
            assertTrue(
                "Current file follows the finger",
                first.left.value < bounds.left.value - width * .25f,
            )
            assertTrue(
                "Neighbor is visible before release",
                second.left.value < bounds.right.value - width * .15f,
            )
            assertEquals(first.right.value, second.left.value, 1f)
            compose.onNodeWithTag("artifact-page:1").assertIsSelected()
            compose.waitUntil(10000) {
                val image = pager.captureToImage().asAndroidBitmap()
                image.getPixel(image.width * 4 / 5, image.height * 55 / 100) == Color.RED
            }
            val preview = pager.captureToImage().asAndroidBitmap()
            assertEquals(
                "Current image is painted",
                Color.BLUE,
                preview.getPixel(preview.width / 5, preview.height * 55 / 100),
            )
            assertEquals(
                "Neighbor image is painted",
                Color.RED,
                preview.getPixel(preview.width * 4 / 5, preview.height * 55 / 100),
            )
            System.getProperty("leo.screenshots.dir")?.let { path ->
                File(path).mkdirs()
                File(path, "artifact-swipe-preview.png").outputStream().use {
                    preview.compress(Bitmap.CompressFormat.PNG, 100, it)
                }
            }
            pager.performTouchInput {
                moveTo(Offset(this.width * .88f, height * .55f), 350)
                up()
            }
            waitForImage(1)
            assertEquals(
                bounds.left.value,
                compose.onNodeWithTag("artifact-page:1").getUnclippedBoundsInRoot().left.value,
                1f,
            )
            pager.performTouchInput { swipeLeft() }
            waitForImage(2)
            pager.performTouchInput {
                down(Offset(this.width * .1f, height * .55f))
                moveTo(Offset(this.width * .52f, height * .55f), 350)
            }
            assertTrue(
                compose.onNodeWithTag("artifact-page:1").getUnclippedBoundsInRoot().right.value >
                    bounds.left.value
            )
            pager.performTouchInput {
                moveTo(Offset(this.width * .88f, height * .55f), 250)
                up()
            }
            waitForImage(1)
        }

    @Test
    fun buttonsSwipesAndRestorationKeepTheRightFileAndStopAtTheEnds(): Unit = withFiles { vm ->
        ArtifactGallery(vm, files.take(3), files[1]) {}
    }
        .run {
            waitForImage(2)
            compose.onNodeWithContentDescription("Suivant").performClick()
            waitForImage(3)
            compose.onNodeWithText("3 / 3").assertExists()
            compose.onNodeWithContentDescription("Suivant").assertIsNotEnabled()
            pager.performTouchInput { swipeLeft() }
            waitForImage(3)
            restoration.emulateSavedInstanceStateRestore()
            waitForImage(3)
            pager.performTouchInput { swipeRight() }
            waitForImage(2)
            compose.onNodeWithContentDescription("Précédent").performClick()
            waitForImage(1)
            pager.performTouchInput { swipeRight() }
            waitForImage(1)
            compose.onNodeWithContentDescription("Précédent").assertIsNotEnabled()
            compose.onNodeWithContentDescription("Enregistrer").assertIsEnabled()
        }

    @Test
    fun zoomedImagePansInsteadOfSwitchingFilesUntilZoomIsReset(): Unit = withFiles { vm ->
        ArtifactGallery(vm, files.take(2), files[0]) {}
    }
        .run {
            waitForImage(1)
            val image =
                compose.onNode(
                    hasContentDescription(files[0].name) and
                        hasAnyAncestor(hasTestTag("artifact-pager"))
                )
            image.performTouchInput {
                pinch(
                    start0 = Offset(width * .4f, centerY),
                    end0 = Offset(width * .15f, centerY),
                    start1 = Offset(width * .6f, centerY),
                    end1 = Offset(width * .85f, centerY),
                    durationMillis = 500,
                )
            }
            pager.performTouchInput { swipeLeft() }
            waitForImage(1)
            image.performTouchInput {
                pinch(
                    start0 = Offset(width * .1f, centerY),
                    end0 = Offset(width * .49f, centerY),
                    start1 = Offset(width * .9f, centerY),
                    end1 = Offset(width * .51f, centerY),
                    durationMillis = 500,
                )
            }
            pager.performTouchInput { swipeLeft() }
            waitForImage(2)
        }

    @Test
    fun transcriptFileRailOpensTheGallery(): Unit = withFiles { vm ->
        ArtifactLinkHost(vm, files.take(2)) { ArtifactStrip(vm, files.take(2)) }
    }
        .run {
            compose.onNodeWithText(files[0].name).performClick()
            waitForImage(1)
            pager.performTouchInput { swipeLeft() }
            waitForImage(2)
            compose.onNodeWithContentDescription("Fermer le fichier").performClick()
            pager.assertDoesNotExist()
            compose.onNodeWithTag("conversation-files").assertExists()
        }

    @Test
    fun aSingleFileIgnoresSwipesAndStillClosesNormally(): Unit = withFiles { vm ->
        var open by remember { mutableStateOf(true) }
        if (open) ArtifactGallery(vm, files.take(1), files[0]) { open = false }
    }
        .run {
            waitForImage(1)
            pager.performTouchInput {
                swipeLeft()
                swipeRight()
            }
            waitForImage(1)
            compose.onNodeWithContentDescription("Suivant").assertDoesNotExist()
            compose.onNodeWithContentDescription("Fermer le fichier").performClick()
            pager.assertDoesNotExist()
        }

    @Test
    fun filesPanelUsesVisibleGroupOrderAndHidesSupersededVersions(): Unit = withFiles { vm ->
        val entries =
            listOf(
                files[0].copy(group = "Plans"),
                files[1].copy(group = "Photos"),
                files[2].copy(group = "Plans"),
                files[0].copy(id = "old", version = 0),
            )
        ArtifactsPanel(vm, entries)
    }
        .run {
            compose.onNodeWithText(files[0].name).performClick()
            waitForImage(1)
            pager.performTouchInput { swipeLeft() }
            waitForImage(3)
            compose.onNodeWithText("2 / 3").assertExists()
            pager.performTouchInput { swipeLeft() }
            waitForImage(2)
        }

    @Test
    fun largeNeighborsWaitUntilSelectedAndDoNotDownloadTheWholeGallery(): Unit = withFiles { vm ->
        val large = files.map { it.copy(size = 20L * 1024 * 1024) }
        ArtifactGallery(vm, large, large[0]) {}
    }
        .run {
            waitForImage(1)
            assertEquals(setOf("1"), downloads.toSet())
            pager.performTouchInput { swipeLeft() }
            waitForImage(2)
            assertEquals(setOf("1", "2"), downloads.toSet())
        }

    private fun waitForImage(index: Int) {
        compose.waitUntil(15000) {
            compose
                .onAllNodes(
                    hasContentDescription(files[index - 1].name) and
                        hasAnyAncestor(hasTestTag("artifact-pager"))
                )
                .fetchSemanticsNodes()
                .isNotEmpty() &&
                compose
                    .onAllNodes(hasContentDescription("Enregistrer") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
        }
        compose.onNodeWithTag("artifact-page:$index").assertIsSelected()
        compose.onNodeWithContentDescription("Enregistrer").assertIsEnabled()
    }

    private fun withFiles(content: @Composable (LeoViewModel) -> Unit): ArtifactSwipeTest {
        // Server is retained through the test by the JUnit rule below.
        fun png(color: Int) =
            ByteArrayOutputStream()
                .apply output@{
                    Bitmap.createBitmap(200, 200, Bitmap.Config.ARGB_8888).apply {
                        eraseColor(color)
                        compress(Bitmap.CompressFormat.PNG, 100, this@output)
                        recycle()
                    }
                }
                .toByteArray()
        val blue = png(Color.BLUE)
        val red = png(Color.RED)
        server.dispatcher =
            object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val path = request.path.orEmpty()
                    if (path.contains("/artifacts/") && path.endsWith("?download=1")) {
                        downloads.add(path.substringBefore('?').substringAfterLast('/'))
                        return MockResponse()
                            .setBody(Buffer().write(if (path.contains("/1?")) blue else red))
                    }
                    return MockResponse()
                        .setBody(
                            when (path) {
                                "/api/session" -> """{"authenticated":true,"csrf":"fixture"}"""
                                "/api/agents" ->
                                    wireJson.encodeToString(listOf(Agent(MAIN_AGENT_ID, "Leo")))
                                "/api/overview",
                                "/api/codex/models",
                                "/api/claude/models" -> "{}"
                                else -> "[]"
                            }
                        )
                }
            }
        val vm =
            LeoViewModel(ApplicationProvider.getApplicationContext<Application>(), MemoryVault())
        restoration.setContent {
            val state by vm.state.collectAsStateWithLifecycle()
            // Restoration recomposes this effect; reconnecting would reset the workspace and
            // rebuild the gallery.
            LaunchedEffect(Unit) {
                vm.state.first { it.ready }
                if (!vm.state.value.session.authenticated) vm.connect(server.url("/").toString())
            }
            LeoTheme { if (state.agents.isNotEmpty()) content(vm) }
        }
        compose.waitUntil(15000) {
            compose.onAllNodesWithTag("artifact-pager").fetchSemanticsNodes().isNotEmpty() ||
                compose
                    .onAllNodesWithTag("conversation-files")
                    .fetchSemanticsNodes()
                    .isNotEmpty() ||
                compose
                    .onAllNodesWithText("Rechercher un fichier")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
        }
        return this
    }

    @get:Rule val server = MockWebServer()
}
