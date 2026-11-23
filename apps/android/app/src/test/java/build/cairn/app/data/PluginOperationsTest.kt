package build.cairn.app.data

import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class PluginOperationsTest {
    private val operationId = "11111111-1111-4111-8111-111111111111"
    private val instanceId = "22222222-2222-4222-8222-222222222222"

    @Test
    fun resourceResolutionContinuesAfterARateLimitedPoll() = runTest {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("""{"operation":"$operationId"}"""))
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"id":"$operationId","state":"running","result":null,"error":null}"""
                    )
            )
            server.enqueue(MockResponse().setResponseCode(429).setBody("""{"error":"busy"}"""))
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"id":"$operationId","state":"completed","error":null,"result":{"kind":"resourceAncestry","resolution":{"resource":{"id":"support","name":"#support","resourceType":"channel"},"ancestors":[]}}}"""
                    )
            )
            server.start()
            val api = CairnApi(server.url("/"), MemoryVault())
            val resolved =
                api.pluginOperation(
                    "/plugins/rules-studio/connections/$instanceId/resources/ancestors?resourceType=channel&id=support",
                    method = "GET",
                )
            assertEquals(
                "#support",
                resolved.jsonObject
                    .getValue("resource")
                    .jsonObject
                    .getValue("name")
                    .jsonPrimitive
                    .content,
            )
            server.takeRequest()
            repeat(3) {
                val poll = server.takeRequest()
                assertEquals("GET", poll.method)
                assertEquals("/api/plugins/packages/operations/$operationId", poll.path)
            }
        }
    }
}
