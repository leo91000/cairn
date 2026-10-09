package build.cairn.app.data

import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

class CairnAccountSignInTest {
    @Test
    fun `GitHub opens the beacon browser handover and exchanges its one use proof`() = runBlocking {
        MockWebServer().use { server ->
            server.start()
            val launcher =
                server
                    .url(
                        "/api/account/oauth/github/native/browser?challenge=one-use&opener=launcher"
                    )
                    .toString()
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"challenge":"one-use","secret":"native-secret","url":"$launcher"}"""
                    )
            )
            server.enqueue(MockResponse().setResponseCode(202).setBody("""{"pending":true}"""))
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"authenticated":true,"csrf":"csrf","account":{"id":"account","email":"alice@example.test"}}"""
                    )
                    .addHeader("Set-Cookie", "cairn_session=cairn; Path=/; HttpOnly")
            )
            val vault = MemoryVault()
            val api = CairnApi(server.url("/"), vault, installationId = "selected")
            val credentials =
                object : CairnCredentials {
                    override suspend fun google(clientId: String, nonce: String): String =
                        error("unused")

                    override suspend fun authenticatePasskey(options: String): String =
                        error("unused")

                    override suspend fun createPasskey(options: String): String = error("unused")

                    override fun openGitHub(url: String) {
                        assertEquals(launcher, url)
                    }
                }
            assertTrue(CairnAccountSignIn(api, credentials).github().authenticated)
            assertEquals("/api/account/oauth/github/start", server.takeRequest().path)
            repeat(2) {
                val finish = server.takeRequest()
                assertEquals("/api/account/oauth/github/native/finish", finish.path)
                assertEquals(
                    "native-secret",
                    wireJson
                        .parseToJsonElement(finish.body.readUtf8())
                        .jsonObject["secret"]
                        ?.jsonPrimitive
                        ?.content,
                )
            }
            assertFalse(vault.values.values.single().contains("native-secret"))
        }
    }

    @Test
    fun `passkey creation sign in and confirmation reuse web contracts with native options`() =
        runTest {
            MockWebServer().use { server ->
                val creation =
                    """{"challenge":"register","options":{"publicKey":{"challenge":"Y2hhbGxlbmdl","rp":{"id":"cairn.example"},"user":{"id":"dXNlcg","name":"alice@example.test"}}}}"""
                val authentication =
                    """{"challenge":"login","options":{"publicKey":{"challenge":"Y2hhbGxlbmdl","rpId":"cairn.example","allowCredentials":[],"userVerification":"required"}}}"""
                server.enqueue(MockResponse().setBody(creation))
                server.enqueue(MockResponse().setResponseCode(201))
                server.enqueue(
                    MockResponse()
                        .setBody(authentication)
                        .addHeader("Set-Cookie", "cairn_passkey=browser; Path=/; HttpOnly")
                )
                server.enqueue(
                    MockResponse()
                        .setBody(
                            """{"authenticated":true,"csrf":"csrf","account":{"id":"account","email":"alice@example.test"}}"""
                        )
                        .addHeader("Set-Cookie", "cairn_session=cairn; Path=/; HttpOnly")
                )
                server.enqueue(MockResponse().setBody(authentication))
                server.enqueue(MockResponse().setResponseCode(204))
                server.start()
                val vault = MemoryVault()
                val api = CairnApi(server.url("/"), vault, installationId = "selected")
                api.csrf = "initial-csrf"
                val credentials =
                    object : CairnCredentials {
                        override suspend fun google(clientId: String, nonce: String): String =
                            error("unused")

                        override fun openGitHub(url: String) = error("unused")

                        override suspend fun createPasskey(options: String): String {
                            val parsed = wireJson.parseToJsonElement(options).jsonObject
                            assertTrue(parsed.containsKey("rp"))
                            assertFalse(parsed.containsKey("publicKey"))
                            return """{"id":"credential","type":"public-key","response":{"attestationObject":"proof"}}"""
                        }

                        override suspend fun authenticatePasskey(options: String): String {
                            val parsed = wireJson.parseToJsonElement(options).jsonObject
                            assertEquals("cairn.example", parsed["rpId"]?.jsonPrimitive?.content)
                            assertEquals(
                                "required",
                                parsed["userVerification"]?.jsonPrimitive?.content,
                            )
                            return """{"id":"credential","type":"public-key","response":{"signature":"proof"}}"""
                        }
                    }
                val signIn = CairnAccountSignIn(api, credentials)
                signIn.createPasskey("Téléphone")
                assertEquals("/api/account/passkeys/register/start", server.takeRequest().path)
                val registration = server.takeRequest()
                assertEquals("/api/account/passkeys/register/finish", registration.path)
                assertEquals("initial-csrf", registration.getHeader("X-CSRF-Token"))
                assertTrue(registration.body.readUtf8().contains("Téléphone"))
                assertTrue(signIn.passkey().authenticated)
                assertEquals("/api/account/passkeys/login/start", server.takeRequest().path)
                val login = server.takeRequest()
                assertEquals("/api/account/passkeys/login/finish", login.path)
                assertEquals("cairn_passkey=browser", login.getHeader("Cookie"))
                signIn.confirmPasskey()
                assertEquals("/api/account/passkeys/reauth/start", server.takeRequest().path)
                val confirmation = server.takeRequest()
                assertEquals("/api/account/passkeys/reauth/finish", confirmation.path)
                assertEquals("csrf", confirmation.getHeader("X-CSRF-Token"))
                assertTrue(vault.values.values.single().startsWith("cairn_session=cairn;"))
            }
        }

    @Test
    fun `Google credential is exchanged only with the Beacon and becomes a Cairn session`() =
        runTest {
            MockWebServer().use { server ->
                server.enqueue(
                    MockResponse()
                        .setBody("""{"challenge":"one-use","nonce":"nonce","clientId":"client"}""")
                        .addHeader("Set-Cookie", "cairn_oauth=browser; Path=/; HttpOnly")
                )
                server.enqueue(
                    MockResponse()
                        .setBody(
                            """{"authenticated":true,"csrf":"csrf","account":{"id":"account","email":"alice@example.test"}}"""
                        )
                        .addHeader("Set-Cookie", "cairn_session=cairn; Path=/; HttpOnly")
                )
                server.start()
                val vault = MemoryVault()
                val api = CairnApi(server.url("/"), vault, installationId = "selected")
                val credentials =
                    object : CairnCredentials {
                        override suspend fun google(clientId: String, nonce: String): String {
                            assertEquals("client", clientId)
                            assertEquals("nonce", nonce)
                            return "provider-token"
                        }

                        override suspend fun authenticatePasskey(options: String): String =
                            error("unused")

                        override suspend fun createPasskey(options: String): String =
                            error("unused")

                        override fun openGitHub(url: String) = error("unused")
                    }
                val session = CairnAccountSignIn(api, credentials).google()
                assertEquals("account", session.account?.id)
                assertEquals("/api/account/oauth/google/start", server.takeRequest().path)
                val exchange = server.takeRequest()
                assertEquals("/api/account/oauth/google/callback", exchange.path)
                assertEquals("cairn_oauth=browser", exchange.getHeader("Cookie"))
                assertTrue(exchange.body.readUtf8().contains("provider-token"))
                assertTrue(vault.values.values.single().startsWith("cairn_session=cairn;"))
                assertFalse(vault.values.values.single().contains("provider-token"))
            }
        }
}
