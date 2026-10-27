package build.cairn.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import build.cairn.app.data.*
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
class PluginProjectPickerTest {
    @get:Rule val compose = createComposeRule()

    private val account =
        PluginConnection(
            id = "acct-1",
            pluginId = "forge",
            name = "Work",
            settings = kotlinx.serialization.json.JsonObject(emptyMap()),
            status = PluginStatus(),
            usage = PluginUsage(),
            enabled = true,
        )
    private val catalog =
        PluginCatalog(
            packages =
                listOf(
                    PluginEntry(
                        id = "forge",
                        active = "1.0.0",
                        manifest =
                            PluginManifest(
                                id = "forge",
                                name = "Forge",
                                instances =
                                    PluginInstances(singular = "Account", plural = "Accounts"),
                                projectSources =
                                    listOf(
                                        PluginProjectSource(
                                            name = "repositories",
                                            title = "Repositories",
                                            description = "Repositories this account can read.",
                                        )
                                    ),
                                version = "1.0.0",
                                apiVersion = 2,
                                capabilities = emptyList(),
                            ),
                        builtin = false,
                        status = "active",
                        setup = null,
                        versions = emptyList(),
                    ),
                    // Not approved: its sources are not offered.
                    PluginEntry(
                        id = "pending",
                        status = "needs_approval",
                        manifest =
                            PluginManifest(
                                id = "pending",
                                name = "Pending",
                                projectSources =
                                    listOf(PluginProjectSource(name = "all", title = "All")),
                                version = "1.0.0",
                                apiVersion = 2,
                                capabilities = emptyList(),
                            ),
                        builtin = false,
                        active = null,
                        setup = null,
                        versions = emptyList(),
                    ),
                ),
            connections =
                listOf(
                    account,
                    PluginConnection(
                        id = "acct-2",
                        pluginId = "forge",
                        name = "Paused",
                        enabled = false,
                        settings = kotlinx.serialization.json.JsonObject(emptyMap()),
                        status = PluginStatus(),
                        usage = PluginUsage(),
                    ),
                    PluginConnection(
                        id = "p1",
                        pluginId = "pending",
                        name = "Pending",
                        settings = kotlinx.serialization.json.JsonObject(emptyMap()),
                        status = PluginStatus(),
                        usage = PluginUsage(),
                        enabled = true,
                    ),
                ),
            capabilities = emptyList(),
        )

    @Test
    fun sourcesComeFromApprovedPackagesWithAnEnabledInstance() {
        val source = catalog.projectSources().single()
        assertEquals("forge/repositories", source.key)
        assertEquals("Forge · Repositories", source.label)
        assertEquals(listOf("acct-1"), source.instances.map { it.id })
        // A source titled with its package name is not repeated.
        val titled = source.copy(source = source.source.copy(title = "Forge repository"))
        assertEquals("Forge repository", titled.label)
        // Without an enabled instance there is nothing to import from.
        assertTrue(
            catalog.copy(connections = catalog.connections.drop(1)).projectSources().isEmpty()
        )
        assertEquals(
            "/plugins/forge/projects?source=repositories&instance=acct%201&page=2",
            pluginProjectsPath("forge", "repositories", "acct 1", 2),
        )
    }

    @Test
    fun searchMatchesNamesDescriptionsAndBadges() {
        val project =
            PluginProject("team/site", "team/site", "Marketing site", badges = listOf("Private"))
        assertTrue(project.matches(""))
        assertTrue(project.matches("SITE"))
        assertTrue(project.matches("marketing"))
        assertTrue(project.matches("private"))
        // Every word counts, in any order, without accents.
        assertTrue(project.matches("site  marketing"))
        assertTrue(project.matches("márketing"))
        assertFalse(project.matches("site rust"))
        assertFalse(project.matches("rust"))
        val decoded =
            wireJson.decodeFromString<PluginProjectPage>(
                """{"items":[{"id":"a/b","name":"a/b","badges":["Fork"],"updatedAt":null,"imported":true,"later":1}],"nextPage":null}"""
            )
        assertEquals(listOf("Fork"), decoded.items.single().badges)
        assertTrue(decoded.items.single().imported)
        assertNull(decoded.nextPage)
    }

    @Test
    fun retryAutoPaginationSearchSelectionAndChange() {
        MockWebServer().use { server ->
            server.enqueue(
                MockResponse().setResponseCode(502).setBody("""{"error":"Forge indisponible"}""")
            )
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"items":[{"id":"team/existing","name":"team/existing","imported":true},{"id":"team/site","name":"team/site","badges":["Vue"]}],"nextPage":2}"""
                    )
            )
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"items":[{"id":"team/selected","name":"team/selected","defaultBranch":"develop","badges":["Private","Rust"],"updatedAt":"2026-09-21T12:00:00Z"}],"nextPage":null}"""
                    )
            )
            server.start()
            val vault =
                object : SessionVault {
                    override fun read(origin: String): String? = null

                    override fun write(origin: String, cookie: String?) {}
                }
            val api = CairnApi(server.url("/"), vault)
            val source = catalog.projectSources().single()
            var selection: PluginProject? = null
            compose.setContent {
                var selected by remember { mutableStateOf("") }
                CairnTheme {
                    Surface(Modifier.fillMaxSize()) {
                        Column {
                            PluginProjectPicker(api, source, account.id, selected) {
                                selection = it
                                selected = it.id
                            }
                        }
                    }
                }
            }
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Réessayer").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Forge indisponible").assertExists()
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
            compose.onNodeWithText("Private").assertExists()
            compose.onNodeWithText("⎇ develop").assertExists()
            screenshot("plugin-project-picker-list")
            compose.onNodeWithText("Rechercher un projet").performTextInput("rust")
            compose.onNodeWithText("team/existing").assertDoesNotExist()
            compose.onNodeWithText("team/site").assertDoesNotExist()
            compose.onNodeWithText("team/selected").performClick()
            compose.runOnIdle { assertEquals("develop", selection?.defaultBranch) }
            compose.onNodeWithText("Changer").assertExists()
            screenshot("plugin-project-picker-selected")
            compose.onNodeWithText("Rechercher un projet").assertDoesNotExist()
            compose.onNodeWithText("Changer").performClick()
            compose.onNodeWithText("team/selected").assertExists()

            // Every page names the source and the instance.
            val paths = List(server.requestCount) { server.takeRequest().path.orEmpty() }
            assertEquals(
                listOf(
                    "/api/plugins/forge/projects?source=repositories&instance=acct-1&page=1",
                    "/api/plugins/forge/projects?source=repositories&instance=acct-1&page=1",
                    "/api/plugins/forge/projects?source=repositories&instance=acct-1&page=2",
                ),
                paths,
            )
        }
    }

    private fun screenshot(name: String) {
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
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
