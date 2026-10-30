package build.cairn.app.ui

import android.app.Activity
import android.app.Application
import android.content.ClipboardManager
import android.content.Intent
import android.graphics.Bitmap
import android.media.ExifInterface
import android.net.Uri
import androidx.activity.ComponentActivity
import androidx.activity.compose.LocalActivityResultRegistryOwner
import androidx.activity.result.ActivityResultRegistry
import androidx.activity.result.ActivityResultRegistryOwner
import androidx.activity.result.contract.ActivityResultContract
import androidx.compose.runtime.*
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.unit.Density
import androidx.core.app.ActivityOptionsCompat
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import build.cairn.app.data.*
import java.io.File
import java.io.IOException
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import okio.Buffer
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class AttachmentViewsTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()
    @get:Rule internal val fileProvider = FileProviderRule()

    private val application = ApplicationProvider.getApplicationContext<Application>()

    @Before
    fun initializeWork() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            application,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
    }

    @After
    fun closeWork() {
        WorkManagerTestInitHelper.closeWorkDatabase()
    }

    private fun viewModel() =
        ViewModelProvider(
            compose.activity,
            viewModelFactory {
                initializer { CairnViewModel(application, MemoryVault(), beaconOrigin = "") }
            },
        )[CairnViewModel::class.java]

    private fun photo(name: String): File =
        File(application.cacheDir, name).apply {
            outputStream().use {
                Bitmap.createBitmap(40, 30, Bitmap.Config.ARGB_8888)
                    .compress(Bitmap.CompressFormat.PNG, 100, it)
            }
        }

    private fun file(name: String, chat: String = "", size: Long = 3 * 1024) =
        ChatAttachment(id = name, chatId = chat, name = name, size = size.toInt())

    private fun image(name: String, chat: String = "") =
        ChatAttachment(
            id = name,
            chatId = chat,
            name = name,
            size = 2048,
            mediaType = "image/png",
            kind = "image",
        )

    @Test
    fun copyingRenamedImagesKeepsTheImageMimeAndOriginalBytes() {
        val bytes = photo("clipboard.png").readBytes()
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path.orEmpty()
                        if (path.endsWith("/stream") || path.endsWith("?preview"))
                            return MockResponse()
                                .setHeader("Content-Type", "image/png")
                                .setBody(Buffer().write(bytes))
                        val body =
                            when {
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path.endsWith("/overview") -> installationOverviewFixture()
                                path.endsWith("/models") -> """{"models":[]}"""
                                else ->
                                    if (
                                        request.requestUrl
                                            ?.encodedPath
                                            ?.endsWith("/api/projects") == true
                                    )
                                        """{"projects":[]}"""
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/agents") ==
                                            true
                                    )
                                        """{"agents":[]}"""
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/tasks") ==
                                            true
                                    )
                                        """{"tasks":[]}"""
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/mcps") ==
                                            true
                                    )
                                        """{"mcps":[]}"""
                                    else "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            server.start()
            val vm = viewModel()
            var item by mutableStateOf(image("capture", "chat"))
            compose.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme { AttachmentActions(vm, item, close = {}) }
            }
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Copier l’image").fetchSemanticsNodes().isNotEmpty() &&
                    vm.state.value.installation != null &&
                    !vm.state.value.busy
            }
            val clipboard = application.getSystemService(ClipboardManager::class.java)
            for (name in listOf("capture", "capture.bin", "capture.jpg")) {
                compose.runOnIdle { item = image(name, "chat") }
                compose.onNodeWithText(name).assertExists()
                compose.onNodeWithText("Copier l’image").performClick()
                compose.waitUntil(10000) { clipboard.primaryClip?.description?.label == name }
                val clip = checkNotNull(clipboard.primaryClip)
                assertTrue(clip.description.hasMimeType("image/png"))
                val uri = clip.getItemAt(0).uri
                assertEquals("image/png", application.contentResolver.getType(uri))
                application.contentResolver.openInputStream(uri)!!.use {
                    assertArrayEquals(bytes, it.readBytes())
                }
            }
            assertNull(vm.state.value.error)
        }
    }

    @Test
    fun documentNamesKeepTheirExtensionsAtLargerFontSizes() {
        val vm = viewModel()
        val names =
            listOf(
                "requirements-documentation.markdown",
                "Brief_refonte_page_tarifs_v3_FINAL_relu.pdf",
            )
        var fontScale by mutableFloatStateOf(1f)
        compose.setContent {
            val density = LocalDensity.current.density
            CompositionLocalProvider(LocalDensity provides Density(density, fontScale)) {
                CairnTheme { MessageAttachments(vm, names.map { file(it) }) }
            }
        }
        for (scale in listOf(1f, 1.3f, 2f)) {
            compose.runOnIdle { fontScale = scale }
            for (name in names) {
                // Accessibility keeps the complete name, while the rendered suffix stays visible.
                compose.onNodeWithText(name).assertExists()
                val suffix = ".${attachmentExtension(name)}"
                val extension = compose.onNodeWithText(suffix, useUnmergedTree = true)
                extension.assertIsDisplayed()
                val layouts = mutableListOf<TextLayoutResult>()
                extension.performSemanticsAction(SemanticsActions.GetTextLayoutResult) { action ->
                    assertTrue(action(layouts))
                }
                val layout = layouts.single()
                assertFalse(layout.multiParagraph.didExceedMaxLines)
                // The paragraph can reserve more width than its glyphs use. Check the actual
                // characters against the rendered bounds instead of that reserved width.
                for (index in suffix.indices) {
                    val bounds = layout.getBoundingBox(index)
                    assertTrue(
                        "$suffix at scale $scale: $bounds within ${layout.size}",
                        bounds.left >= -1f &&
                            bounds.right <= layout.size.width + 1f &&
                            bounds.top >= -1f &&
                            bounds.bottom <= layout.size.height + 1f,
                    )
                }
                for (line in 0 until layout.lineCount) assertFalse(layout.isLineEllipsized(line))
            }
        }
    }

    @Test
    @Config(sdk = [28])
    fun recentPhotoThumbnailsBoundPanoramasOnOlderAndroid() = runBlocking {
        for ((width, height) in listOf(12000 to 24, 24 to 12000)) {
            val panorama = File(application.cacheDir, "recent-$width.png")
            val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
            panorama.outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
            bitmap.recycle()
            val thumbnail = checkNotNull(photoThumbnail(application, Uri.fromFile(panorama)))
            assertTrue(thumbnail.width <= 512)
            assertTrue(thumbnail.height <= 512)
            assertTrue(thumbnail.byteCount <= 512 * 512 * 4)
            thumbnail.recycle()
        }
    }

    @Test
    fun galleryShowsAndDismissesActionFailuresInsideItsWindow() {
        val vm = viewModel()
        val document = file("notes.txt")
        val local = File(application.cacheDir, "notes.txt").apply { writeText("Notes") }
        compose.setContent {
            CairnTheme {
                AttachmentGallery(
                    vm,
                    listOf(document),
                    document,
                    local = mapOf(document.id to local.path),
                    close = {},
                )
            }
        }
        compose.runOnIdle { vm.report(IOException("Le téléchargement a échoué")) }
        compose
            .onNode(
                hasText("Le téléchargement a échoué") and
                    hasAnyAncestor(hasTestTag("attachment-viewer"))
            )
            .assertIsDisplayed()
        compose.onNodeWithContentDescription("Masquer l’erreur").performClick()
        compose.onNodeWithText("Le téléchargement a échoué").assertDoesNotExist()
    }

    @Test
    fun panoramicThumbnailsHaveBoundedDecodedDimensions() {
        for ((width, height) in listOf(12000 to 24, 24 to 12000)) {
            val panorama = File(application.cacheDir, "panorama-$width.png")
            val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
            panorama.outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
            bitmap.recycle()
            val thumbnail = checkNotNull(decodeSampled(panorama, 128))
            assertTrue(thumbnail.width <= 256)
            assertTrue(thumbnail.height <= 256)
            assertTrue(thumbnail.byteCount <= 256 * 256 * 4)
        }
    }

    @Test
    fun cameraOrientationIsAppliedAtThumbnailAndViewerSizes() {
        val photo = File(application.cacheDir, "portrait-camera.jpg")
        photo.outputStream().use {
            Bitmap.createBitmap(400, 300, Bitmap.Config.ARGB_8888)
                .compress(Bitmap.CompressFormat.JPEG, 90, it)
        }
        ExifInterface(photo.path).apply {
            setAttribute(
                ExifInterface.TAG_ORIENTATION,
                ExifInterface.ORIENTATION_ROTATE_90.toString(),
            )
            saveAttributes()
        }
        for (target in listOf(128, 2048)) {
            val decoded = checkNotNull(decodeSampled(photo, target))
            assertTrue("Portrait orientation at size $target", decoded.height > decoded.width)
            decoded.recycle()
        }
    }

    @Test
    fun summariesAndThumbnailsStayWithinTheirInstallationSession() {
        MockWebServer().use { first ->
            MockWebServer().use { second ->
                val requests = CopyOnWriteArrayList<String>()
                val bytes = photo("scoped-preview.png").readBytes()
                for ((server, label) in
                    listOf(first to "Première installation", second to "Seconde installation")) {
                    server.dispatcher =
                        object : Dispatcher() {
                            override fun dispatch(request: RecordedRequest): MockResponse {
                                val path = request.path.orEmpty()
                                requests += "$label:$path"
                                if (path.endsWith("?preview"))
                                    return MockResponse().setBody(Buffer().write(bytes))
                                val body =
                                    when {
                                        path.endsWith("?summary") ->
                                            """{"lines":2,"excerpt":"$label"}"""
                                        path == "/api/account/session" ->
                                            beaconAccountFixture("fixture")
                                        path == "/api/installations" -> beaconInstallationsFixture()
                                        path.endsWith("/overview") -> installationOverviewFixture()
                                        path.endsWith("/models") -> """{"models":[]}"""
                                        else ->
                                            if (
                                                request.requestUrl
                                                    ?.encodedPath
                                                    ?.endsWith("/api/projects") == true
                                            )
                                                """{"projects":[]}"""
                                            else if (
                                                request.requestUrl
                                                    ?.encodedPath
                                                    ?.endsWith("/api/agents") == true
                                            )
                                                """{"agents":[]}"""
                                            else if (
                                                request.requestUrl
                                                    ?.encodedPath
                                                    ?.endsWith("/api/tasks") == true
                                            )
                                                """{"tasks":[]}"""
                                            else if (
                                                request.requestUrl
                                                    ?.encodedPath
                                                    ?.endsWith("/api/mcps") == true
                                            )
                                                """{"mcps":[]}"""
                                            else "[]"
                                    }
                                return MockResponse()
                                    .setHeader("Content-Type", "application/json")
                                    .setBody(body)
                            }
                        }
                    server.start()
                }
                val vm = viewModel()
                compose.setContent {
                    LaunchedEffect(Unit) {
                        vm.state.first { it.ready }
                        vm.connect(first.url("/").toString())
                    }
                    CairnTheme {
                        MessageAttachments(
                            vm,
                            listOf(image("restored.png", "chat"), file("notes.txt", "chat")),
                        )
                    }
                }
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Première installation")
                        .fetchSemanticsNodes()
                        .isNotEmpty() &&
                        !vm.state.value.busy &&
                        requests.any {
                            it.startsWith("Première installation:") && it.endsWith("?preview")
                        }
                }
                compose.runOnIdle { vm.perform { vm.connect(second.url("/").toString()) } }
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Seconde installation")
                        .fetchSemanticsNodes()
                        .isNotEmpty() &&
                        !vm.state.value.busy &&
                        requests.any {
                            it.startsWith("Seconde installation:") && it.endsWith("?preview")
                        }
                }
                compose.onNodeWithText("Première installation").assertDoesNotExist()
            }
        }
    }

    @Test
    fun externalFileTypesStayDistinctFromTheTextViewer() {
        for ((name, mime) in
            listOf(
                "export.csv" to "text/csv",
                "data.json" to "application/json",
                "page.html" to "text/html",
            )) {
            val item = file(name).copy(mediaType = mime)
            assertEquals("code" to "text/plain", viewerType(item))
            assertEquals(mime, attachmentMediaType(item))
        }
    }

    @Test
    fun savingRestoresThePendingFileAfterRecreation() {
        val vm = viewModel()
        val original =
            File(application.cacheDir, "rotation-source.txt").apply {
                writeText("Document à conserver")
            }
        val destination = File(application.cacheDir, "saved-after-rotation.txt").apply { delete() }
        val item = file(original.name, size = original.length())
        val registry =
            object : ActivityResultRegistry() {
                var requested: Int? = null

                override fun <I, O> onLaunch(
                    requestCode: Int,
                    contract: ActivityResultContract<I, O>,
                    input: I,
                    options: ActivityOptionsCompat?,
                ) {
                    requested = requestCode
                }
            }
        val owner =
            object : ActivityResultRegistryOwner {
                override val activityResultRegistry = registry
            }
        val restoration = StateRestorationTester(compose)
        restoration.setContent {
            CompositionLocalProvider(LocalActivityResultRegistryOwner provides owner) {
                CairnTheme {
                    AttachmentGallery(
                        vm,
                        listOf(item),
                        item,
                        mapOf(item.id to original.path),
                        close = {},
                    )
                }
            }
        }
        compose.onNodeWithContentDescription("Enregistrer").performClick()
        val request = checkNotNull(registry.requested)
        restoration.emulateSavedInstanceStateRestore()
        compose.runOnIdle {
            registry.dispatchResult(
                request,
                Activity.RESULT_OK,
                Intent().setData(Uri.fromFile(destination)),
            )
        }
        compose.waitUntil(10000) {
            destination.exists() && destination.readText() == original.readText()
        }
        assertNull(vm.state.value.error)
    }

    @Test
    fun composerTrayShowsEachFileStateAndRemovesTheChosenOne() {
        val vm = viewModel()
        val capture = photo("capture.png")
        val removed = mutableListOf<String>()
        val dismissed = mutableListOf<String>()
        compose.setContent {
            CairnTheme {
                ComposerAttachments(
                    vm,
                    listOf(
                        DraftAttachment(image("capture.png"), capture.path),
                        DraftAttachment(file("Brief_refonte_page_tarifs_v3_FINAL_relu.pdf")),
                        DraftAttachment(file("export.csv")),
                    ),
                    listOf(
                        RejectedAttachment(
                            "video",
                            "demo.mov",
                            "video/quicktime",
                            "Trop lourd (14,2 Mo > 10 Mo)",
                        )
                    ),
                    progress = mapOf("capture.png" to 0.4f),
                    errors = mapOf("export.csv" to "Réseau indisponible"),
                    remove = { removed += it },
                    dismiss = { dismissed += it },
                )
            }
        }
        compose.onNodeWithContentDescription("Envoi de capture.png : 40 %").assertExists()
        // The full name stays available even when the card shortens it in the middle.
        compose.onNodeWithText("Brief_refonte_page_tarifs_v3_FINAL_relu.pdf").assertExists()
        compose.onNodeWithText("PDF · 3 Ko").assertExists()
        compose
            .onNodeWithContentDescription(
                "export.csv : échec de l’envoi, Réseau indisponible",
                substring = true,
            )
            .assertExists()
        compose.onNodeWithText("Échec de l’envoi · Réseau indisponible").assertExists()
        compose.onNodeWithContentDescription("capture.png").assertExists()

        // The row scrolls sideways to the cards beyond the screen edge.
        compose.onNodeWithTag("composer-attachments").performScrollToIndex(2)
        compose.onNodeWithContentDescription("Retirer export.csv").performClick()
        compose.onNodeWithTag("composer-attachments").performScrollToIndex(3)
        compose.onNodeWithText("Trop lourd (14,2 Mo > 10 Mo)").assertExists()
        compose.onNodeWithContentDescription("Retirer demo.mov").performClick()
        assertEquals(listOf("export.csv"), removed)
        assertEquals(listOf("video"), dismissed)
    }

    @Test
    fun sentFilesFormAMosaicCardsAndActionsToAttachThemAgain() {
        val vm = viewModel()
        val reused = mutableListOf<ChatAttachment>()
        val archive = file("backup.zip", chat = "chat")
        compose.setContent {
            CairnTheme {
                CompositionLocalProvider(
                    LocalAttachmentReuse provides AttachmentReuse("chat") { reused += it }
                ) {
                    MessageAttachments(
                        vm,
                        (1..6).map { image("photo-$it.jpg") } + listOf(file("brief.pdf"), archive),
                    )
                }
            }
        }
        // Four tiles show; the last counts the two others.
        compose.onNodeWithText("+2").assertExists()
        compose.onNodeWithContentDescription("photo-4.jpg").assertExists()
        compose.onAllNodesWithContentDescription("photo-5.jpg").assertCountEquals(0)
        compose.onNodeWithText("brief.pdf").assertExists()
        compose.onNodeWithText("backup.zip").assertExists()
        compose.onNodeWithText("ZIP · 3 Ko").assertExists()

        compose.onNodeWithText("backup.zip").performTouchInput { longClick() }
        compose.onNodeWithTag("attachment-actions").assertExists()
        compose.onNodeWithText("Enregistrer").assertExists()
        compose.onNodeWithText("Partager").assertExists()
        compose.onAllNodesWithText("Copier l’image").assertCountEquals(0)
        compose.onNodeWithText("Réutiliser dans un nouveau message").performClick()
        assertEquals(listOf(archive), reused)
    }

    @Test
    fun reuseIsOnlyOfferedForFilesOfTheOpenConversation() {
        val vm = viewModel()
        compose.setContent {
            CairnTheme {
                CompositionLocalProvider(
                    LocalAttachmentReuse provides AttachmentReuse("other") {}
                ) {
                    MessageAttachments(vm, listOf(file("backup.zip", chat = "chat")))
                }
            }
        }
        compose.onNodeWithText("backup.zip").performTouchInput { longClick() }
        compose.onNodeWithText("Enregistrer").assertExists()
        compose.onAllNodesWithText("Réutiliser dans un nouveau message").assertCountEquals(0)
    }

    @Test
    fun viewerPagesThroughTheDraftWithAFilmstrip() {
        val vm = viewModel()
        val source = "const answer = 42;"
        val notes = File(application.cacheDir, "notes.ts").apply { writeText(source) }
        assertEquals("code" to "text/plain", viewerType(file("notes.ts")))
        val capture = photo("vue.png")
        compose.setContent {
            CairnTheme {
                ComposerAttachments(
                    vm,
                    listOf(
                        DraftAttachment(image("vue.png"), capture.path),
                        DraftAttachment(file("notes.ts", size = notes.length()), notes.path),
                    ),
                    emptyList(),
                    emptyMap(),
                    emptyMap(),
                    remove = {},
                    dismiss = {},
                )
            }
        }
        compose.onNodeWithText("notes.ts").performClick()
        compose.onNodeWithTag("attachment-viewer").assertExists()
        compose.onNodeWithText("TS · 18 o · 2 / 2").assertExists()
        compose.waitUntil(10000) {
            compose.onAllNodesWithText(source).fetchSemanticsNodes().isNotEmpty()
        }
        // The filmstrip jumps back to the image.
        compose.onNode(hasClickLabel("vue.png")).performClick()
        compose.waitUntil(10000) {
            compose.onAllNodesWithText("PNG · 2 Ko · 1 / 2").fetchSemanticsNodes().isNotEmpty()
        }
        compose.onNodeWithContentDescription("Fermer").performClick()
        compose.onAllNodesWithTag("attachment-viewer").assertCountEquals(0)
    }

    private fun hasClickLabel(label: String) =
        SemanticsMatcher("click label $label") {
            it.config
                .getOrElseNullable(androidx.compose.ui.semantics.SemanticsActions.OnClick) {
                    null
                }
                ?.label == label
        }
}
