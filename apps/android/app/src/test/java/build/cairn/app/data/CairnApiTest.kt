package build.cairn.app.data

import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.encodeToJsonElement
import okhttp3.Cookie
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

class MemoryVault : SessionVault {
    val values = mutableMapOf<String, String>()

    override fun read(origin: String) = values[origin]

    override fun write(origin: String, cookie: String?) {
        if (cookie == null) values.remove(origin) else values[origin] = cookie
    }
}

class CairnApiTest {
    @Test
    fun `sign in challenges stay in memory and on the fixed official origin`() {
        val origin = "https://cairn.example/".toHttpUrl()
        val vault = MemoryVault()
        val cookies = SessionCookies(origin, vault)
        cookies.saveFromResponse(
            origin,
            listOf(Cookie.parse(origin, "cairn_passkey=challenge; Path=/; Secure")!!),
        )
        assertEquals("challenge", cookies.loadForRequest(origin).single().value)
        assertTrue(vault.values.isEmpty())
        assertTrue(cookies.loadForRequest("https://other.example/".toHttpUrl()).isEmpty())
        assertTrue(SessionCookies(origin, vault).loadForRequest(origin).isEmpty())
        cookies.clear()
        assertTrue(cookies.loadForRequest(origin).isEmpty())
    }

    @Test
    fun `official MCP grants and consent stay on the official service`() = runTest {
        MockWebServer().use { server ->
            repeat(2) { server.enqueue(MockResponse().setBody("{}")) }
            server.start()
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "shared")
            api.csrf = "account-csrf"

            api.request("GET", "/tokens")
            assertEquals("/api/installations/shared/tokens", server.takeRequest().path)

            api.request("POST", "/mcp/oauth/preview", body("client_id" to "assistant"))
            val preview = server.takeRequest()
            assertEquals("/api/mcp/oauth/preview", preview.path)
            assertEquals("account-csrf", preview.getHeader("X-CSRF-Token"))
        }
    }

    @Test
    fun `anonymous official session accepts null account and csrf`() {
        val session =
            wireJson.decodeFromString<Session>(
                """{"authenticated":false,"csrf":null,"account":null,"installations":[]}"""
            )
        assertFalse(session.authenticated)
    }

    @Test
    fun `selected installation scopes reads writes files and streams but not account calls`() =
        runTest {
            MockWebServer().use { server ->
                repeat(4) { server.enqueue(MockResponse().setBody("{}")) }
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault(), installationId = "shared")
                api.csrf = "account-csrf"
                api.request("GET", "/chats?before=4")
                assertEquals(
                    "/api/installations/shared/api/chats?before=4",
                    server.takeRequest().path,
                )
                api.request("POST", "/chats", body("prompt" to "Hello"))
                val write = server.takeRequest()
                assertEquals("/api/installations/shared/api/chats", write.path)
                assertEquals("account-csrf", write.getHeader("X-CSRF-Token"))
                api.request("GET", "/account/session")
                assertEquals("/api/account/session", server.takeRequest().path)
                assertEquals(
                    "/api/installations/shared/api/chats/chat/stream?after=12",
                    api.url("/chats/chat/stream?after=12").encodedPath +
                        "?" +
                        api.url("/chats/chat/stream?after=12").encodedQuery,
                )
                val file = java.io.File.createTempFile("cairn-fixture", ".txt")
                try {
                    api.download("/runs/run/artifacts/file", file)
                    assertEquals(
                        "/api/installations/shared/api/runs/run/artifacts/file",
                        server.takeRequest().path,
                    )
                } finally {
                    file.delete()
                }
            }
        }

    @Test
    fun `email code uses the official origin and restores the account cookie`() = runTest {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("{}"))
            server.enqueue(
                MockResponse()
                    .setBody("""{"authenticated":true,"csrf":"account-csrf"}""")
                    .addHeader(
                        "Set-Cookie",
                        "cairn_session=account-fixture; Path=/; HttpOnly; Max-Age=3600",
                    )
            )
            server.enqueue(
                MockResponse().setBody("""{"authenticated":true,"csrf":"account-csrf"}""")
            )
            server.start()
            val vault = MemoryVault()
            val api = CairnApi(server.url("/"), vault)
            api.request("POST", "/account/email-code", body("email" to "member@example.test"))
            val codeRequest = server.takeRequest()
            assertEquals("/api/account/email-code", codeRequest.path)
            assertEquals(
                server.url("/").toString().removeSuffix("/"),
                codeRequest.getHeader("Origin"),
            )
            api.send<Session>(
                "POST",
                "/account/verify",
                body("challenge" to "fixture-challenge", "code" to "12345678"),
            )
            server.takeRequest()
            assertTrue(
                CairnApi(server.url("/"), vault).get<Session>("/account/session").authenticated
            )
            assertEquals("cairn_session=account-fixture", server.takeRequest().getHeader("Cookie"))
        }
    }

    @Test
    fun `portraits use authenticated bounded transfers and preserve old agent compatibility`() =
        runTest {
            MockWebServer().use { server ->
                server.enqueue(
                    MockResponse()
                        .setBody("""{"authenticated":true,"csrf":"test-csrf"}""")
                        .addHeader(
                            "Set-Cookie",
                            "cairn_session=portrait-session; Path=/; HttpOnly; Max-Age=3600",
                        )
                )
                server.enqueue(MockResponse().setBody("portrait-bytes"))
                server.enqueue(
                    MockResponse()
                        .setBody(
                            """{"id":"agent-1","name":"Reviewer","avatar":{"status":"ready","revision":"new","url":"/api/agents/agent-1/avatar?v=new"}}"""
                        )
                )
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault())
                api.csrf = api.send<Session>("POST", "/account/verify").csrf.orEmpty()
                server.takeRequest()
                assertArrayEquals(
                    "portrait-bytes".toByteArray(),
                    api.agentPortrait("agent-1", "revision"),
                )
                val get = server.takeRequest()
                assertEquals("/api/agents/agent-1/avatar?v=revision", get.path)
                assertEquals("cairn_session=portrait-session", get.getHeader("Cookie"))
                val saved = api.uploadAgentPortrait("agent-1", byteArrayOf(1, 2, 3))
                assertEquals("ready", saved.avatar?.status)
                val put = server.takeRequest()
                assertEquals("PUT", put.method)
                assertEquals("test-csrf", put.getHeader("X-CSRF-Token"))
                assertArrayEquals(byteArrayOf(1, 2, 3), put.body.readByteArray())
                assertNull(
                    wireJson.decodeFromString<Agent>("""{"id":"legacy","name":"Legacy"}""").avatar
                )
                assertThrows(IllegalArgumentException::class.java) {
                    readPortraitBytes(java.io.ByteArrayInputStream(ByteArray(20)), 10)
                }
            }
        }

    @Test
    fun `production requires a clean HTTPS origin`() {
        assertEquals("https://cairn.example/", serverOrigin("https://cairn.example").toString())
        listOf(
                "http://cairn.example",
                "https://owner:password@cairn.example",
                "https://cairn.example/api",
                "https://cairn.example/?token=x",
                "https://cairn.example/#x",
            )
            .forEach {
                assertThrows(IllegalArgumentException::class.java) { serverOrigin(it) }
            }
        assertEquals("http://10.0.2.2:4310/", serverOrigin("http://10.0.2.2:4310", true).toString())
        assertThrows(IllegalArgumentException::class.java) {
            serverOrigin("http://192.168.1.1", true)
        }
    }

    @Test
    fun `session cookies are scoped to scheme host port and expiration`() {
        val origin = "https://cairn.example/".toHttpUrl()
        val vault = MemoryVault()
        val jar = SessionCookies(origin, vault)
        jar.saveFromResponse(
            origin,
            listOf(
                Cookie.parse(
                    origin,
                    "cairn_session=test-session; Path=/; Secure; HttpOnly; Max-Age=3600",
                )!!
            ),
        )
        assertEquals(1, jar.loadForRequest(origin).size)
        listOf("https://other.example/", "http://cairn.example/", "https://cairn.example:8443/")
            .forEach {
                assertTrue(jar.loadForRequest(it.toHttpUrl()).isEmpty())
            }
        assertEquals(1, SessionCookies(origin, vault).loadForRequest(origin).size)
        jar.saveFromResponse(
            origin,
            listOf(Cookie.parse(origin, "cairn_session=; Path=/; Max-Age=0")!!),
        )
        assertTrue(jar.loadForRequest(origin).isEmpty())
        assertTrue(vault.values.isEmpty())
    }

    @Test
    fun `login uses server cookie and CSRF on mutations then clears logout cookie`() = runTest {
        MockWebServer().use { server ->
            server.enqueue(
                MockResponse()
                    .setBody("""{"authenticated":true,"csrf":"test-csrf"}""")
                    .addHeader(
                        "Set-Cookie",
                        "cairn_session=test-session; Path=/; HttpOnly; Max-Age=3600",
                    )
            )
            server.enqueue(MockResponse().setBody("""{"id":"agent-1","name":"Reviewer"}"""))
            server.enqueue(
                MockResponse()
                    .setBody("""{"ok":true}""")
                    .addHeader("Set-Cookie", "cairn_session=; Path=/; Max-Age=0")
            )
            server.start()
            val vault = MemoryVault()
            val api = CairnApi(server.url("/"), vault)
            val session =
                api.send<Session>(
                    "POST",
                    "/account/verify",
                    body("challenge" to "fixture-challenge", "code" to "12345678"),
                )
            api.csrf = session.csrf.orEmpty()
            api.send<Agent>(
                "POST",
                "/agents",
                wireJson.encodeToJsonElement(Agent(name = "Reviewer")),
            )
            assertEquals("/api/account/verify", server.takeRequest().path)
            val mutation = server.takeRequest()
            assertEquals("cairn_session=test-session", mutation.getHeader("Cookie"))
            assertEquals("test-csrf", mutation.getHeader("X-CSRF-Token"))
            assertTrue(mutation.body.readUtf8().contains("Reviewer"))
            api.request("POST", "/account/logout")
            assertTrue(vault.values.isEmpty())
        }
    }

    @Test
    fun `redirects never forward a session to another origin`() = runTest {
        MockWebServer().use { server ->
            MockWebServer().use { other ->
                other.start()
                server.start()
                server.enqueue(
                    MockResponse().setResponseCode(302).addHeader("Location", other.url("/steal"))
                )
                val api = CairnApi(server.url("/"), MemoryVault())
                try {
                    api.request("GET", "/account/session")
                    fail("Expected an API error")
                } catch (e: ApiException) {
                    assertEquals(302, e.status)
                }
                assertEquals(0, other.requestCount)
            }
        }
    }

    @Test
    fun `a lost mutation response is not automatically replayed`() = runTest {
        MockWebServer().use { server ->
            server.enqueue(
                MockResponse()
                    .setSocketPolicy(okhttp3.mockwebserver.SocketPolicy.DISCONNECT_AFTER_REQUEST)
            )
            server.enqueue(MockResponse().setBody("{}"))
            server.start()
            val api = CairnApi(server.url("/"), MemoryVault())
            try {
                api.request("POST", "/tasks", body("name" to "Single task"))
                fail("The ambiguous network failure must be reported to the owner")
            } catch (_: java.io.IOException) {
                assertEquals(1, server.requestCount)
            }
        }
    }

    @Test
    fun `expired sessions expose status and server error without treating it as data`() = runTest {
        MockWebServer().use { server ->
            server.enqueue(
                MockResponse().setResponseCode(401).setBody("""{"error":"Please sign in."}""")
            )
            server.start()
            try {
                CairnApi(server.url("/"), MemoryVault()).get<List<Task>>("/tasks")
                fail("Expected unauthorized")
            } catch (e: ApiException) {
                assertEquals(401, e.status)
                assertEquals("Please sign in.", e.message)
            }
        }
    }

    @Test
    fun `models accept list and detail wire formats and preserve task controls`() {
        val run =
            wireJson.decodeFromString<Run>(
                """{"id":"r","status":"running","taskName":"Review","agentName":"Agent","usage":null,"startedAt":null}"""
            )
        assertEquals("Review", run.title)
        assertTrue(run.active)
        val task =
            Task(
                name = "Review",
                prompt = "Review changes",
                cron = null,
                enabled = false,
                archived = true,
                worktree = false,
                skills = listOf("global/review"),
                tags = listOf("release"),
            )
        assertEquals(task, wireJson.decodeFromString<Task>(wireJson.encodeToString(task)))
        val audit =
            wireJson.decodeFromString<Audit>(
                """{"id":1,"created_at":1234,"action":"task.saved","detail":"{}"}"""
            )
        assertEquals(1234L, audit.at)
    }

    @Test
    fun `current server null project and skill selections preserve inherited access`() {
        val task =
            wireJson.decodeFromString<Task>(
                """{"id":"task","name":"Review","projectId":null,"skills":null}"""
            )
        assertNull(task.projectId)
        assertNull(task.skills)
        assertEquals(task, wireJson.decodeFromString<Task>(wireJson.encodeToString(task)))
        val run =
            wireJson.decodeFromString<Run>(
                """{"id":"run","status":"interrupted","projectId":null,"snapshot":{"project":null,"projects":[{"id":"project","name":"Cairn"}]},"resumeAvailable":true}"""
            )
        assertNull(run.snapshot.project)
        assertEquals("Cairn", run.snapshot.projects.single().name)
        assertTrue(run.resumeAvailable)
        val agent =
            wireJson.decodeFromString<Agent>(
                """{"id":"agent","name":"Restricted","access":{"projects":[],"skills":null,"mcps":["mcp"],"mcpTools":{"mcp":["read"]},"github":false,"sandbox":"read-only"}}"""
            )
        val edited =
            wireJson.decodeFromString<Agent>(wireJson.encodeToString(agent.copy(name = "Renamed")))
        assertEquals(agent.access, edited.access)
        assertEquals(emptyList<String>(), edited.access.projects)
        assertNull(edited.access.skills)
        assertFalse(edited.access.github)
    }

    @Test
    fun `OAuth links cannot switch origin or contain ambiguous parameters`() {
        val origin = "https://cairn.example/".toHttpUrl()
        assertEquals(
            "client",
            authorizationParameters(
                "https://cairn.example/authorize?client_id=client&scope=read%20run",
                origin,
            )["client_id"],
        )
        listOf(
                "https://evil.example/authorize?client_id=a",
                "https://cairn.example/api/tokens",
                "https://cairn.example/authorize?scope=read&scope=manage",
                "https://cairn.example:444/authorize",
                "https://user@cairn.example/authorize",
            )
            .forEach {
                assertThrows(IllegalArgumentException::class.java) {
                    authorizationParameters(it, origin)
                }
            }
    }

    @Test
    fun `Markdown file links resolve only known artifacts on the connected origin`() {
        val origin = "https://cairn.example/".toHttpUrl()
        val file = Deliverable("file", "run", key = "report")
        assertEquals(
            file,
            artifactForLink("/api/runs/run/artifacts/file?download=1", origin, listOf(file)),
        )
        assertEquals(
            file,
            artifactForLink(
                "https://cairn.example/api/runs/run/artifacts/file",
                origin,
                listOf(file),
            ),
        )
        listOf(
                "//other.example/api/runs/run/artifacts/file",
                "https://user@cairn.example/api/runs/run/artifacts/file",
                "https://cairn.example:444/api/runs/run/artifacts/file",
                "/api/runs/other/artifacts/file",
                "file:///api/runs/run/artifacts/file",
                "javascript:alert(1)",
            )
            .forEach { link ->
                assertNull(artifactForLink(link, origin, listOf(file)))
            }
    }

    @Test
    fun `artifact links can locate an older run without trusting external URLs`() {
        val origin = "https://cairn.example/".toHttpUrl()
        assertEquals(
            "/runs/older/artifacts/file",
            artifactPathForLink("/api/runs/older/artifacts/file?download=1#top", origin),
        )
        for (link in
            listOf(
                "//other.example/api/runs/run/artifacts/file",
                "https://user@cairn.example/api/runs/run/artifacts/file",
                "/api/runs/run/artifacts/file/preview",
                "javascript:alert(1)",
                "file:///api/runs/run/artifacts/file",
            )) {
            assertNull(artifactPathForLink(link, origin))
        }
    }
}
