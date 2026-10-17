package build.cairn.app.ui

import android.app.Application
import android.graphics.Bitmap
import androidx.activity.ComponentActivity
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
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
class AttachmentViewsTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    private val application = ApplicationProvider.getApplicationContext<Application>()

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
        ChatAttachment(name, chat, name, size)

    private fun image(name: String, chat: String = "") =
        ChatAttachment(name, chat, name, 2048, "image/png", "image")

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
        val notes = File(application.cacheDir, "notes.txt").apply { writeText("Première ligne") }
        val capture = photo("vue.png")
        compose.setContent {
            CairnTheme {
                ComposerAttachments(
                    vm,
                    listOf(
                        DraftAttachment(image("vue.png"), capture.path),
                        DraftAttachment(file("notes.txt", size = notes.length()), notes.path),
                    ),
                    emptyList(),
                    emptyMap(),
                    emptyMap(),
                    remove = {},
                    dismiss = {},
                )
            }
        }
        compose.onNodeWithText("notes.txt").performClick()
        compose.onNodeWithTag("attachment-viewer").assertExists()
        compose.onNodeWithText("TXT · 15 o · 2 / 2").assertExists()
        compose.waitUntil(10000) {
            compose.onAllNodesWithText("Première ligne").fetchSemanticsNodes().isNotEmpty()
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
