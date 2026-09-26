package dev.leo.manager.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import dev.leo.manager.data.*
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Rule
import org.junit.Test
import org.junit.Assert.*

abstract class NodePlacementCases {
    @get:Rule val compose = createComposeRule()

    @Test fun ownerCanPreferOrPinAnAuthorizedNodeAndSeeBackupAge() {
        MockWebServer().use { server ->
            val saved=CopyOnWriteArrayList<JsonObject>()
            server.dispatcher=object:Dispatcher() {
                override fun dispatch(request:RecordedRequest):MockResponse {
                    val path=request.path!!.substringBefore('?')
                    val body=when {
                        path=="/api/nodes/placement/run" && request.method=="PUT" -> { saved+=wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject; "{}" }
                        path=="/api/nodes/placement/run" -> """{"nodes":[{"id":"node","name":"Mon serveur","status":"online"}],"pinnedNodeId":null,"preferredNodeId":null}"""
                        path=="/api/session" -> """{"authenticated":true,"csrf":"fixture"}"""
                        path=="/api/overview" -> "{}"
                        else -> "[]"
                    }
                    return MockResponse().setBody(body)
                }
            }
            server.start()
            val vault=object:SessionVault {
                private val values=mutableMapOf<String,String>()
                override fun read(origin:String)=values[origin]
                override fun write(origin:String,cookie:String?) {if(cookie==null)values.remove(origin)else values[origin]=cookie}
            }
            val vm=LeoViewModel(ApplicationProvider.getApplicationContext<Application>(),vault)
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {vm.state.first {it.ready}; vm.connect(server.url("/").toString())}
                LeoTheme {if(state.session.authenticated) Page {NodePlacement(vm,Run(id="run",status="succeeded",nodeId="node",backup=NodeBackup(capturedAt=System.currentTimeMillis()-120000,status="ready")))}}
            }
            compose.waitUntil(20000) {compose.onAllNodesWithText("Mon serveur").fetchSemanticsNodes().isNotEmpty()}
            compose.onNodeWithText("Automatique").performClick()
            compose.onNodeWithText("Préférer une node").performClick()
            compose.onNodeWithText("Enregistrer le placement").performScrollTo().performClick()
            compose.waitUntil(10000) {saved.size==1}
            assertEquals("node",saved[0]["preferredNodeId"]?.jsonPrimitive?.content)
            assertEquals(JsonNull,saved[0]["pinnedNodeId"])
            compose.onNodeWithText("Préférer une node").performScrollTo().performClick()
            compose.onNodeWithText("Fixer à une node").performClick()
            compose.onNodeWithText("Enregistrer le placement").performScrollTo().performClick()
            compose.waitUntil(10000) {saved.size==2}
            assertEquals("node",saved[1]["pinnedNodeId"]?.jsonPrimitive?.content)
            assertEquals(JsonNull,saved[1]["preferredNodeId"])
            compose.onNodeWithText("Dernier état restaurable",substring=true).assertExists()
            compose.onNodeWithText("Déplacer maintenant").assertIsNotEnabled()
        }
    }

    @Test fun ownerCanEnrollConfigureAndRevokeANode() {
        MockWebServer().use { server ->
            val writes = CopyOnWriteArrayList<Pair<String, JsonObject>>()
            var revoked = false
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val path = request.path!!.substringBefore('?')
                    if (request.method in listOf("POST", "PUT") && path.startsWith("/api/nodes")) {
                        val body = request.body.readUtf8().ifBlank { "{}" }
                        writes += path to wireJson.parseToJsonElement(body).jsonObject
                    }
                    val body = when {
                        path == "/api/nodes/enrollments" -> """{"code":"fixture-enrollment","expiresAt":4102444800000,"installCommand":"curl https://fixture.invalid/install | bash"}"""
                        path == "/api/nodes/node/revoke" -> { revoked = true; "{}" }
                        path == "/api/nodes/node" -> "{}"
                        path == "/api/nodes" -> """[{"id":"node","name":"Serveur test","status":"${if (revoked) "revoked" else "online"}","revoked":$revoked,"accepting":true,"limits":{"cpu":4,"memoryMiB":8192,"diskMiB":65536}}]"""
                        path == "/api/session" -> """{"authenticated":true,"csrf":"fixture"}"""
                        path == "/api/overview" -> "{}"
                        else -> "[]"
                    }
                    return MockResponse().setBody(body)
                }
            }
            server.start()
            val vault = object : SessionVault {
                private val values = mutableMapOf<String, String>()
                override fun read(origin: String) = values[origin]
                override fun write(origin: String, cookie: String?) { if (cookie == null) values.remove(origin) else values[origin] = cookie }
            }
            val vm = LeoViewModel(ApplicationProvider.getApplicationContext<Application>(), vault)
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) { vm.state.first { it.ready }; vm.connect(server.url("/").toString()) }
                LeoTheme { if (state.session.authenticated) NodesScreen(vm, state) }
            }
            compose.waitUntil(20000) { compose.onAllNodesWithText("Nom de la machine").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Nom de la machine").performTextInput("Nouvelle node")
            compose.onNodeWithText("Créer un code d’inscription").performScrollTo().performClick()
            compose.waitUntil(10000) { compose.onAllNodesWithText("fixture-enrollment").fetchSemanticsNodes().isNotEmpty() }
            assertEquals("Nouvelle node", writes.first().second["name"]?.jsonPrimitive?.content)
            compose.onNodeWithText("Masquer").performScrollTo().performClick()
            compose.onNodeWithText("Configurer", substring = false).performScrollTo().performClick()
            compose.onNodeWithText("Plafond CPU").performTextReplacement("3")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { writes.any { it.first == "/api/nodes/node" } }
            assertEquals(3, writes.first { it.first == "/api/nodes/node" }.second["limits"]!!.jsonObject["cpu"]!!.jsonPrimitive.int)
            compose.onNodeWithText("Révoquer", substring = false).performScrollTo().performClick()
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(10000) { compose.onAllNodesWithText("Révoquée").fetchSemanticsNodes().isNotEmpty() }
            assertTrue(writes.any { it.first == "/api/nodes/node/revoke" })
        }
    }

}
