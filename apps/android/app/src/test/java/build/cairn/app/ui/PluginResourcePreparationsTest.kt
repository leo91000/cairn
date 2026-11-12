package build.cairn.app.ui

import build.cairn.app.data.CairnApi
import build.cairn.app.data.MemoryVault
import build.cairn.contracts.ListPluginResourcesInput
import build.cairn.contracts.PluginResource
import build.cairn.contracts.PluginResourceRef
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.test.runTest
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.*
import org.junit.Test

class PluginResourcePreparationsTest {
    private val first = "11111111-1111-4111-8111-111111111111"
    private val second = "22222222-2222-4222-8222-222222222222"
    private val third = "33333333-3333-4333-8333-333333333333"

    private fun resource(id: String) = """{"id":"$id","name":"#$id","resourceType":"channel"}"""

    private fun resolution(id: String) =
        """{"resource":${resource(id)},"ancestors":[{"id":"guild","name":"Team","resourceType":"server"}]}"""

    @Test
    fun largePagesAndSelectedRootsShareFourSlotsAcrossPickers() = runTest {
        val active = AtomicInteger()
        val peak = AtomicInteger()
        val refused = AtomicInteger()
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val count = active.incrementAndGet()
                        peak.updateAndGet { maxOf(it, count) }
                        try {
                            if (count > 4) {
                                refused.incrementAndGet()
                                return MockResponse()
                                    .setResponseCode(429)
                                    .setBody("""{"error":"busy"}""")
                            }
                            Thread.sleep(30)
                            val id = request.requestUrl!!.queryParameter("id")
                            val body =
                                if (id == null)
                                    """{"items":[${(0..11).joinToString { resource(it.toString()) }}],"nextPage":null}"""
                                else resolution(id)
                            return MockResponse().setBody(body)
                        } finally {
                            active.decrementAndGet()
                        }
                    }
                }
            server.start()
            val api = CairnApi(server.url("/"), MemoryVault())
            val pickers =
                listOf(first, second).map { instance ->
                    async {
                        val page =
                            listPluginGrantResources(
                                api,
                                ListPluginResourcesInput(instance, 1, "large-pages", "", "channel"),
                            )
                        resolvePluginResourcePage(api, "large-pages", instance, page.items)
                    }
                }
            val selected =
                (0..7).map { id ->
                    async {
                        resolvePluginResourcePath(
                            api,
                            "large-pages",
                            third,
                            PluginResourceRef(id.toString(), "channel"),
                        )
                    }
                }
            pickers.awaitAll().forEach { page ->
                assertEquals(12, page.size)
                assertTrue(page.all { it.second == "Team › #${it.first.id}" })
            }
            assertEquals(8, selected.awaitAll().size)
            assertTrue(peak.get() in 1..4)
            assertEquals(0, refused.get())
        }
    }

    @Test
    fun busyPathsRetryWithoutCancellingSuccessfulRowsAndCanBeRecovered() = runTest {
        val busy = AtomicBoolean(true)
        val attempts = ConcurrentHashMap<String, AtomicInteger>()
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val id = request.requestUrl!!.queryParameter("id")!!
                        val count =
                            attempts.computeIfAbsent(id) { AtomicInteger() }.incrementAndGet()
                        if ((id == "0" && busy.get()) || (id == "1" && count == 1))
                            return MockResponse()
                                .setResponseCode(429)
                                .setBody("""{"error":"busy"}""")
                        return MockResponse().setBody(resolution(id))
                    }
                }
            server.start()
            val api = CairnApi(server.url("/"), MemoryVault())
            val items =
                (0..7).map { PluginResource(it.toString(), "#$it", resourceType = "channel") }
            val rows = resolvePluginResourcePage(api, "partial-page", first, items)
            assertEquals(7, rows.count { it.second != null })
            assertNull(rows.first().second)
            assertEquals(5, attempts["0"]!!.get())
            assertEquals(2, attempts["1"]!!.get())
            busy.set(false)
            val recovered = resolvePluginResourcePage(api, "partial-page", first, items)
            assertTrue(recovered.all { it.second != null })
            assertEquals(6, attempts["0"]!!.get())
            assertEquals(1, attempts["2"]!!.get())
        }
    }
}
