package build.cairn.app.data

import build.cairn.contracts.ImportSkillInput
import build.cairn.contracts.ListPluginResourcesInput
import build.cairn.contracts.ListSourceProjectsInput
import build.cairn.contracts.ResolvePluginResourceInput
import build.cairn.contracts.importSkill
import build.cairn.contracts.listPluginResources
import build.cairn.contracts.listSourceProjects
import build.cairn.contracts.resolvePluginResource
import kotlinx.coroutines.test.runTest
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class ResourceOperationsTest {
    @Test
    fun generatedImportsAwaitTheManagerResult() = runTest {
        MockWebServer().use { server ->
            server.start()
            server.enqueue(MockResponse().setBody("""{"operation":"job"}"""))
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"id":"job","state":"completed","result":{"kind":"skillImport","imported":{"imported":null,"candidates":["one","two"]}},"error":null}"""
                    )
            )
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "i")
            val result = api.importSkill(ImportSkillInput.A(scope = "global", upload = "up"))
            assertNull(result.imported)
            assertEquals(listOf("one", "two"), result.candidates)
            assertEquals("/api/installations/i/api/skills/import", server.takeRequest().path)
            assertEquals(
                "/api/installations/i/api/plugins/packages/operations/job",
                server.takeRequest().path,
            )
        }
    }

    @Test
    fun generatedSourceReadsPollWithoutSendingGetBodies() = runTest {
        MockWebServer().use { server ->
            server.start()
            server.enqueue(MockResponse().setBody("""{"operation":"job"}"""))
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"id":"job","state":"completed","result":{"kind":"sourceProjects","page":{"items":[],"nextPage":null}},"error":null}"""
                    )
            )
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "i")
            val page =
                api.listSourceProjects(
                    ListSourceProjectsInput(
                        pluginId = "forge",
                        source = "repos",
                        instance = "instance",
                    )
                )
            assertEquals(emptyList<Any>(), page.items)
            val request = server.takeRequest()
            assertEquals("GET", request.method)
            assertEquals(0L, request.bodySize)
            assertEquals("GET", server.takeRequest().method)
        }
    }

    @Test
    fun generatedResourceReadsPollAndDecodePagesAndAncestors() = runTest {
        for (resolve in listOf(false, true)) {
            MockWebServer().use { server ->
                server.start()
                server.enqueue(MockResponse().setBody("""{"operation":"job"}"""))
                val item =
                    """{"resourceType":"repository","id":"repo","name":"Repository","parent":null}"""
                val value =
                    if (resolve)
                        """{"kind":"resourceAncestry","resolution":{"resource":$item,"ancestors":[]}}"""
                    else """{"kind":"resourcePage","page":{"items":[$item],"nextPage":null}}"""
                server.enqueue(
                    MockResponse()
                        .setBody(
                            """{"id":"job","state":"completed","result":$value,"error":null}"""
                        )
                )
                val api = CairnApi(server.url("/"), MemoryVault(), installationId = "i")
                if (resolve) {
                    val result =
                        api.resolvePluginResource(
                            ResolvePluginResourceInput(
                                id = "repo",
                                instanceId = "00000000-0000-4000-8000-000000000001",
                                pluginId = "forge",
                                resourceType = "repository",
                            )
                        )
                    assertEquals("repo", result.resource.id)
                    assertEquals(emptyList<Any>(), result.ancestors)
                } else {
                    val result =
                        api.listPluginResources(
                            ListPluginResourcesInput(
                                instanceId = "00000000-0000-4000-8000-000000000001",
                                pluginId = "forge",
                                resourceType = "repository",
                            )
                        )
                    assertEquals("repo", result.items.single().id)
                    assertNull(result.nextPage)
                }
                val start = server.takeRequest()
                assertEquals("GET", start.method)
                assertEquals(0L, start.bodySize)
                assertEquals(
                    "/api/installations/i/api/plugins/packages/operations/job",
                    server.takeRequest().path,
                )
            }
        }
    }

    @Test
    fun revisionConflictsKeepTheirStatusAfterPreparation() = runTest {
        MockWebServer().use { server ->
            server.start()
            server.enqueue(MockResponse().setBody("""{"operation":"job"}"""))
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"id":"job","state":"failed","result":null,"error":"Reload the skill","errorStatus":409}"""
                    )
            )
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "i")
            val error =
                runCatching { api.pluginOperation("/skills/import") }.exceptionOrNull()
                    as ApiException
            assertEquals(409, error.status)
            assertEquals("Reload the skill", error.message)
        }
    }
}
