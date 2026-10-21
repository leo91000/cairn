package build.cairn.app.data

import kotlinx.coroutines.test.runTest
import kotlinx.serialization.encodeToString
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

class PluginCatalogueTest {
    private fun card(id: String) =
        PluginCardEntry(
            id,
            false,
            "needs_approval",
            null,
            versionCount = 0,
            summary = PluginInstanceSummary(0, emptyMap(), 0, 0),
        )

    private fun page(items: List<PluginCardEntry>, cursor: String? = null) =
        MockResponse()
            .setBody(
                wireJson.encodeToString(
                    PluginCards(
                        packageReads = true,
                        instanceReads = true,
                        items = items,
                        nextCursor = cursor,
                    )
                )
            )

    @Test
    fun completesAllCardPagesIncludingAnEmptyIntermediatePage() = runTest {
        MockWebServer().use { server ->
            server.enqueue(page(listOf(card("one")), "a/b"))
            server.enqueue(page(emptyList(), "last"))
            server.enqueue(page(listOf(card("two"))))
            server.start()
            val result = CairnApi(server.url("/"), MemoryVault()).pluginCards()
            assertEquals(listOf("one", "two"), result.items.map { it.id })
            assertNull(result.nextCursor)
            assertEquals("/api/plugins/packages", server.takeRequest().path)
            assertEquals("/api/plugins/packages?cursor=a%2Fb", server.takeRequest().path)
            assertEquals("/api/plugins/packages?cursor=last", server.takeRequest().path)
        }
    }

    @Test
    fun aRepeatedCursorCannotBecomeACompleteCatalogue() = runTest {
        MockWebServer().use { server ->
            server.enqueue(page(listOf(card("one")), "next"))
            server.enqueue(page(emptyList(), "next"))
            server.start()
            try {
                CairnApi(server.url("/"), MemoryVault()).pluginCards()
                fail("Repeated cursor must fail")
            } catch (e: IllegalStateException) {
                assertTrue(e.message.orEmpty().contains("changé"))
            }
            assertEquals(2, server.requestCount)
        }
    }

    @Test
    fun malformedOrDeniedPagesNeverLookLikeAnEmptyCatalogue() = runTest {
        for (body in
            listOf("{}", """{"packageReads":true,"instanceReads":true,"items":[{"id":"one"}]}""")) {
            MockWebServer().use { server ->
                server.enqueue(MockResponse().setBody(body))
                server.start()
                val error = runCatching {
                    CairnApi(server.url("/"), MemoryVault()).pluginCards()
                }
                    .exceptionOrNull()
                assertNotNull("Malformed response must fail", error)
                assertEquals(1, server.requestCount)
            }
        }
        MockWebServer().use { server ->
            server.enqueue(page(listOf(card("one")), "next"))
            server.enqueue(MockResponse().setResponseCode(403).setBody("""{"error":"Denied"}"""))
            server.start()
            try {
                CairnApi(server.url("/"), MemoryVault()).pluginCards()
                fail("Failed page must not return an incomplete list")
            } catch (e: ApiException) {
                assertEquals(403, e.status)
            }
            assertEquals(2, server.requestCount)
        }
    }

    @Test
    fun olderHostCannotTurnGlobalResourceTotalsIntoAPackageAgentCount() = runTest {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(404))
            server.enqueue(
                MockResponse()
                    .setBody(
                        wireJson.encodeToString(
                            PluginCatalog(
                                packages =
                                    listOf(
                                        PluginEntry(id = "old", status = "active", active = "1")
                                    ),
                                connections =
                                    listOf(
                                        PluginConnection(
                                            id = "instance",
                                            pluginId = "old",
                                            enabled = false,
                                            agentIds = listOf("one"),
                                        )
                                    ),
                                resourceGrantCounts = mapOf("two" to 3),
                            )
                        )
                    )
            )
            server.start()
            val result = CairnApi(server.url("/"), MemoryVault()).pluginCards()
            assertFalse(result.packageReads)
            assertEquals(
                PluginInstanceSummary(1, mapOf("paused" to 1), 0, null),
                result.items.single().summary,
            )
            assertEquals("/api/plugins/packages", server.takeRequest().path)
            assertEquals("/api/plugins", server.takeRequest().path)
        }
    }

    @Test
    fun reviewLoadsVersionMetadataWithoutDecodingItAsTheReviewedManifest() = runTest {
        MockWebServer().use { server ->
            server.enqueue(
                MockResponse()
                    .setBody(wireJson.encodeToString(PluginEntry(id = "plugin", active = "1")))
            )
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"items":[{"version":"1","digest":"old","approved":true}],"nextCursor":"1"}"""
                    )
            )
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"items":[{"version":"2","digest":"new","approved":false},{"version":"bad","error":"Invalid module"}]}"""
                    )
            )
            server.start()
            val result = CairnApi(server.url("/"), MemoryVault()).pluginReview("plugin")
            assertTrue(result.lazy)
            assertEquals(listOf("1", "2", "bad"), result.entry.versions.map { it.version })
            assertNull(result.entry.versions[1].manifest)
            assertEquals("/api/plugins/packages/plugin", server.takeRequest().path)
            assertEquals("/api/plugins/packages/plugin/versions", server.takeRequest().path)
            assertEquals(
                "/api/plugins/packages/plugin/versions?cursor=1",
                server.takeRequest().path,
            )
        }
    }
}
