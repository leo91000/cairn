package dev.leo.manager.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import dev.leo.manager.data.*
import java.io.File
import java.time.Instant
import okhttp3.mockwebserver.*
import org.junit.*
import org.junit.Assert.*
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class GithubRepositoryPickerTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun retryAutoPaginationFilterSelectionAndChange() {
        MockWebServer().use { server ->
            server.enqueue(
                MockResponse().setResponseCode(502).setBody("""{"error":"GitHub indisponible"}""")
            )
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"repositories":[{"fullName":"team/existing","name":"existing","owner":"team","imported":true},{"fullName":"team/site","name":"site","language":"Vue"}],"nextPage":2}"""
                    )
            )
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"repositories":[{"fullName":"team/selected","name":"selected","defaultBranch":"develop","private":true,"language":"Rust","stars":1200}],"nextPage":null}"""
                    )
            )
            server.start()
            val vault =
                object : SessionVault {
                    override fun read(origin: String): String? = null

                    override fun write(origin: String, cookie: String?) {}
                }
            val api = LeoApi(server.url("/"), vault)
            var selection: GithubRepository? = null
            compose.setContent {
                var selected by remember { mutableStateOf("") }
                LeoTheme {
                    Surface(Modifier.fillMaxSize()) {
                        Column {
                            GithubRepositoryPicker(api, selected) {
                                selection = it
                                selected = it.fullName
                            }
                        }
                    }
                }
            }
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Réessayer").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Réessayer").performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("team/existing").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("team/existing").assertIsNotEnabled()
            compose.onNodeWithText("Déjà ajouté").assertExists()
            // The short first page leaves the list end visible, so the next page loads on its own.
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("team/selected").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("★ 1.2k").assertExists()
            screenshot("github-picker-list")
            compose.onNodeWithText("Privés · 1").performClick()
            compose.onNodeWithText("team/site").assertDoesNotExist()
            compose
                .onNodeWithText("Rechercher un dépôt, un propriétaire, un langage")
                .performTextInput("rust")
            compose.onNodeWithText("team/existing").assertDoesNotExist()
            compose.onNodeWithText("team/selected").performClick()
            compose.runOnIdle { assertEquals("develop", selection?.defaultBranch) }
            compose.onNodeWithText("Changer").assertExists()
            screenshot("github-picker-selected")
            compose
                .onNodeWithText("Rechercher un dépôt, un propriétaire, un langage")
                .assertDoesNotExist()
            compose.onNodeWithText("Changer").performClick()
            compose.onNodeWithText("team/selected").assertExists()
        }
    }

    private fun screenshot(name: String) {
        val dir = System.getProperty("leo.screenshots.dir") ?: return
        File(dir).mkdirs()
        File(dir, "$name.png").outputStream().use {
            compose
                .onRoot()
                .captureToImage()
                .asAndroidBitmap()
                .compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
        }
    }

    @Test
    fun describesRecentActivityInFrench() {
        val now = Instant.parse("2026-09-24T12:00:00Z")
        assertEquals("Mis à jour il y a 3 jours", updated("2026-09-21T12:00:00Z", now))
        assertEquals("Mis à jour il y a 1 heure", updated("2026-09-24T11:00:00Z", now))
        assertEquals("Mis à jour il y a 2 mois", updated("2026-07-20T12:00:00Z", now))
        assertNull(updated("", now))
    }
}
