package dev.leo.manager.ui

import android.app.Application
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.core.app.ApplicationProvider
import dev.leo.manager.data.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule

abstract class LeoAccountCases {
    @get:Rule val compose = createComposeRule()

    protected open fun sessionVault(application: Application): SessionVault = AccountFixtureVault()

    fun switchingToASharedInstallationHidesManagementAndRestoresTheSelection() {
        MockWebServer().use { server ->
            val installations =
                """[{"id":"selection-home","name":"Maison","role":"owner","online":true},{"id":"selection-work","name":"Bureau","role":"member","online":true}]"""
            val session =
                """{"authenticated":true,"csrf":"selection-csrf","account":{"id":"selection-person","email":"member@example.test"},"installations":$installations}"""
            val paths = java.util.concurrent.CopyOnWriteArrayList<String>()
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path.orEmpty().substringBefore('?')
                        paths += path
                        val response =
                            when (path) {
                                "/api/account/session" -> session
                                "/api/installations" -> installations
                                else -> {
                                    val installation = path.split('/').getOrNull(3)
                                    if (installation !in listOf("selection-home", "selection-work"))
                                        return MockResponse().setResponseCode(404)
                                    val resource =
                                        path.substringAfter("/api/installations/$installation/api")
                                    if (
                                        installation == "selection-work" &&
                                            resource in
                                                setOf(
                                                    "/mcps",
                                                    "/accounts",
                                                    "/onepassword",
                                                    "/nodes",
                                                    "/settings",
                                                    "/tokens",
                                                    "/audit",
                                                )
                                    )
                                        return MockResponse()
                                            .setResponseCode(403)
                                            .setBody("""{"error":"Owner only"}""")
                                    when (resource) {
                                        "/agents" ->
                                            """[{"id":"same-agent","name":"Agent $installation"}]"""
                                        "/overview",
                                        "/codex/models",
                                        "/claude/models" -> "{}"
                                        "/accounts" -> """{"accounts":[]}"""
                                        "/chats/stream" ->
                                            return MockResponse()
                                                .setHeader("Content-Type", "text/event-stream")
                                                .setBody(
                                                    "event: batch\ndata: {\"state\":{\"chats\":[]},\"events\":[],\"cursor\":0}\n\n"
                                                )
                                        else -> "[]"
                                    }
                                }
                            }
                        return MockResponse().setBody(response)
                    }
                }
            server.start()
            val application = ApplicationProvider.getApplicationContext<Application>()
            val origin = server.url("/").toString()
            val vault = sessionVault(application)
            vault.write(origin, "leo_session=selection-fixture; Path=/; Max-Age=3600; HttpOnly")
            val model = mutableStateOf(LeoViewModel(application, vault, officialOrigin = ""))
            compose.setContent {
                val vm = model.value
                key(vm) {
                    LaunchedEffect(Unit) {
                        vm.state.first { it.ready }
                        vm.perform { connect(origin) }
                    }
                    LeoTheme { LeoApp(vm = vm) }
                }
            }
            compose.waitUntil(30000) {
                compose
                    .onAllNodesWithText("Maison · En ligne")
                    .fetchSemanticsNodes()
                    .isNotEmpty() && !model.value.state.value.busy
            }
            compose.onNodeWithContentDescription("Atelier").performClick()
            compose.onNodeWithText("Agents").performClick()
            compose.onNodeWithText("Agent selection-home").performScrollTo().assertIsDisplayed()
            compose.onNodeWithText("Créer un agent").performScrollTo().assertIsDisplayed()
            compose.onNodeWithContentDescription("Choisir une installation").performClick()
            compose.onNodeWithText("Bureau · En ligne").performClick()
            compose.waitUntil(30000) {
                compose
                    .onAllNodesWithText("Bureau · En ligne")
                    .fetchSemanticsNodes()
                    .isNotEmpty() && !model.value.state.value.busy
            }
            compose.onNodeWithText("Agent selection-home").assertDoesNotExist()
            compose.onNodeWithContentDescription("Atelier").performClick()
            compose.onNodeWithText("Connexions").assertDoesNotExist()
            compose.onNodeWithText("Nodes et stockage").assertDoesNotExist()
            compose.onNodeWithText("MCP").assertDoesNotExist()
            compose.onNodeWithText("Agents").performClick()
            compose.onNodeWithText("Agent selection-work").performScrollTo().assertIsDisplayed()
            compose.onNodeWithText("Créer un agent").assertDoesNotExist()
            compose.onNodeWithContentDescription("Modifier").assertDoesNotExist()
            compose.onNodeWithContentDescription("Supprimer").assertDoesNotExist()
            assertFalse(
                paths.any {
                    it.startsWith("/api/installations/selection-work/api/") &&
                        it.substringAfter("/api/installations/selection-work/api/")
                            .substringBefore('/') in
                            setOf(
                                "mcps",
                                "accounts",
                                "onepassword",
                                "nodes",
                                "settings",
                                "tokens",
                                "audit",
                            )
                }
            )
            compose.runOnIdle {
                model.value = LeoViewModel(application, vault, officialOrigin = "")
            }
            compose.waitUntil(30000) {
                compose
                    .onAllNodesWithText("Bureau · En ligne")
                    .fetchSemanticsNodes()
                    .isNotEmpty() && !model.value.state.value.busy
            }
            assertEquals("selection-work", model.value.state.value.installation?.id)
        }
    }

    fun emailCodeOpensTheOnlyInstallationAndLogoutRevokesTheAccount() {
        MockWebServer().use { server ->
            var signedIn = false
            val accountSession =
                """{"authenticated":true,"csrf":"account-csrf","account":{"id":"person","email":"member@example.test"},"installations":[{"id":"home","name":"Maison","role":"owner","online":true}]}"""
            val paths = java.util.concurrent.CopyOnWriteArrayList<String>()
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path.orEmpty().substringBefore('?')
                        paths += path
                        val result =
                            when (path) {
                                "/api/account/session" ->
                                    if (signedIn) accountSession
                                    else
                                        """{"authenticated":false,"csrf":null,"account":null,"installations":[]}"""
                                "/api/account/email-code" -> {
                                    assertEquals(
                                        server.url("/").toString().removeSuffix("/"),
                                        request.getHeader("Origin"),
                                    )
                                    assertEquals(
                                        """{"email":"member@example.test"}""",
                                        request.body.readUtf8(),
                                    )
                                    """{"challenge":"email-fixture-challenge"}"""
                                }
                                "/api/account/verify" -> {
                                    val input = request.body.readUtf8()
                                    assertEquals(
                                        "email-fixture-challenge",
                                        wireJson
                                            .parseToJsonElement(input)
                                            .jsonObject["challenge"]
                                            ?.jsonPrimitive
                                            ?.content,
                                    )
                                    if (!input.contains("12345678"))
                                        return MockResponse()
                                            .setResponseCode(401)
                                            .setBody("""{"error":"Code invalide ou expiré"}""")
                                    signedIn = true
                                    return MockResponse()
                                        .addHeader(
                                            "Set-Cookie",
                                            "leo_session=account-fixture; Path=/; HttpOnly; Max-Age=3600",
                                        )
                                        .setBody(accountSession)
                                }
                                "/api/installations" ->
                                    """[{"id":"home","name":"Maison","role":"owner","online":true}]"""
                                "/api/account/logout" -> {
                                    assertEquals("account-csrf", request.getHeader("X-CSRF-Token"))
                                    assertEquals(
                                        "leo_session=account-fixture",
                                        request.getHeader("Cookie"),
                                    )
                                    signedIn = false
                                    return MockResponse()
                                        .addHeader("Set-Cookie", "leo_session=; Path=/; Max-Age=0")
                                        .setBody("{}")
                                }
                                else -> {
                                    if (!path.startsWith("/api/installations/home/api/"))
                                        return MockResponse()
                                            .setResponseCode(404)
                                            .setBody("""{"error":"Not an official endpoint"}""")
                                    assertEquals(
                                        "leo_session=account-fixture",
                                        request.getHeader("Cookie"),
                                    )
                                    when (path.removePrefix("/api/installations/home/api")) {
                                        "/overview",
                                        "/codex/models",
                                        "/claude/models" -> "{}"
                                        "/accounts" -> """{"accounts":[]}"""
                                        "/chats/stream" ->
                                            return MockResponse()
                                                .setHeader("Content-Type", "text/event-stream")
                                                .setBody(
                                                    "event: snapshot\ndata: {\"state\":{\"chats\":[]},\"cursor\":0}\n\n"
                                                )
                                        else -> "[]"
                                    }
                                }
                            }
                        return MockResponse().setBody(result)
                    }
                }
            server.start()
            val application = ApplicationProvider.getApplicationContext<Application>()
            val vault = sessionVault(application)
            vault.write(server.url("/").toString(), null)
            val vm = LeoViewModel(application, vault, officialOrigin = "")
            compose.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.perform { connect(server.url("/").toString()) }
                }
                LeoTheme { LeoApp(vm = vm) }
            }
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("Adresse e-mail").fetchSemanticsNodes().isNotEmpty() &&
                    !vm.state.value.busy
            }
            compose.onNodeWithText("Adresse du serveur").assertDoesNotExist()
            compose.onNodeWithText("Mot de passe").assertDoesNotExist()
            compose.onNodeWithText("Adresse e-mail").performTextInput("member@example.test")
            compose.onNodeWithText("Recevoir un code").performClick()
            compose.waitUntil(30000) {
                compose
                    .onAllNodesWithText("Code reçu par e-mail")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Code reçu par e-mail").performTextInput("000000")
            compose.onNodeWithText("Se connecter").performClick()
            compose.waitUntil(30000) {
                compose
                    .onAllNodesWithText("Code invalide ou expiré")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Code reçu par e-mail").performTextReplacement("12345678")
            compose.onNodeWithText("Se connecter").performClick()
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("Maison · En ligne").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithContentDescription("Choisir une installation").assertDoesNotExist()
            assertTrue(vault.read(server.url("/").toString()).orEmpty().contains("account-fixture"))
            assertTrue(paths.contains("/api/installations/home/api/agents"))
            compose.onNodeWithContentDescription("Atelier").performClick()
            compose.onNodeWithText("Se déconnecter").performScrollTo().performClick()
            compose.onNodeWithText("Se déconnecter ?").assertIsDisplayed()
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(30000) {
                compose.onAllNodesWithText("Adresse e-mail").fetchSemanticsNodes().isNotEmpty()
            }
            assertNull(vault.read(server.url("/").toString()))
            assertTrue(paths.contains("/api/account/logout"))
        }
    }
}

private class AccountFixtureVault : SessionVault {
    private var cookie: String? = null

    override fun read(origin: String) = cookie

    override fun write(origin: String, cookie: String?) {
        this.cookie = cookie
    }
}
