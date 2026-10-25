package build.cairn.app.data

import kotlinx.coroutines.test.runTest
import kotlinx.serialization.encodeToString
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

class AgentPluginDirectoryTest {
    private fun response(value: String) = MockResponse().setBody(value)

    private val card =
        """{"id":"custom","builtin":false,"active":"1","status":"active","versionCount":1,"presentation":{"name":"Custom","description":"Package","version":"1","capabilities":[],"needsWorker":false,"delegates":false,"agentAccess":true,"agentAccessDescription":"Package access help","contributions":{"commands":0,"tools":0,"instructions":false,"view":false,"test":false,"form":false,"collections":1,"operations":2}}}"""

    private fun instance(id: String, plugin: String = "custom") =
        """{"id":"$id","pluginId":"$plugin","name":"$id","enabled":false,"agentId":"delegate","agentIds":["root"],"resourceAgentIds":["granular"],"status":{"state":"needs_review"}}"""

    @Test
    fun completesEveryCompactPageAndLoadsOnlyTheSelectedManifest() = runTest {
        MockWebServer().use { server ->
            server.enqueue(
                response(
                    """{"packageReads":true,"agentReads":true,"items":[$card],"nextCursor":"custom"}"""
                )
            )
            server.enqueue(
                response(
                    """{"items":[{"id":"unavailable","builtin":false,"active":null,"status":"invalid","versionCount":0}]}"""
                )
            )
            server.enqueue(response("""{"items":[${instance("one")}],"nextCursor":"one"}"""))
            server.enqueue(response("""{"items":[${instance("two", "removed-package")}]}"""))
            server.enqueue(response("""{"items":[{"id":"one","count":3}],"nextCursor":"one"}"""))
            server.enqueue(response("""{"items":[{"id":"two","count":1}]}"""))
            server.enqueue(
                response(
                    """{"id":"custom","builtin":false,"active":"1","status":"active","manifest":{"id":"custom","name":"Custom"}}"""
                )
            )
            server.start()
            val api = CairnApi(server.url("/"), MemoryVault())
            val directory = api.agentPluginDirectory()
            assertEquals(listOf("custom", "unavailable"), directory.packages.map { it.id })
            assertEquals(listOf("one", "two"), directory.connections.map { it.id })
            assertEquals("delegate", directory.connections.first().agentId)
            assertEquals("needs_review", directory.connections.first().status.state)
            assertEquals(mapOf("one" to 3, "two" to 1), directory.resourceGrantCounts)
            assertEquals("Package access help", directory.packages.first().agentAccessDescription)
            assertEquals(6, server.requestCount)
            assertEquals("custom", api.agentPluginManifest(directory, "custom").id)
            val paths = (0 until server.requestCount).map { server.takeRequest().path }
            assertFalse(paths.contains("/api/plugins"))
            assertEquals("/api/plugins/packages/custom", paths.last())
        }
    }

    @Test
    fun aSupportedReadFailureCannotBecomeAnOlderOrPartialDirectory() = runTest {
        MockWebServer().use { server ->
            server.enqueue(response("""{"packageReads":true,"agentReads":true,"items":[$card]}"""))
            server.enqueue(response("""{"items":[${instance("one")}],"nextCursor":"one"}"""))
            server.enqueue(
                MockResponse().setResponseCode(503).setBody("""{"error":"Later page offline"}""")
            )
            server.start()
            val error = runCatching {
                CairnApi(server.url("/"), MemoryVault()).agentPluginDirectory()
            }
                .exceptionOrNull()
            assertTrue(error is ApiException)
            assertEquals(503, (error as ApiException).status)
            assertEquals(3, server.requestCount)
        }
    }

    @Test
    fun explicitOlderHostKeepsUnknownResourceTotalsAndItsChosenSchema() = runTest {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(404))
            server.enqueue(
                response(
                    wireJson.encodeToString(
                        PluginCatalog(
                            packages =
                                listOf(
                                    PluginEntry(
                                        "old",
                                        active = "1",
                                        manifest = PluginManifest(id = "old", name = "Old"),
                                    )
                                )
                        )
                    )
                )
            )
            server.start()
            val api = CairnApi(server.url("/"), MemoryVault())
            val directory = api.agentPluginDirectory()
            assertNull(directory.resourceGrantCounts)
            assertEquals("old", api.agentPluginManifest(directory, "old").id)
            assertEquals(2, server.requestCount)
        }
    }
}
