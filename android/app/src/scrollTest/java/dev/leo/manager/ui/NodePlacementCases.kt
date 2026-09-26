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
                        path=="/api/nodes/placement/run" -> """{"nodes":[{"id":"node","name":"Mon serveur","status":"online"},{"id":"other","name":"Autre serveur","status":"online"}],"pinnedNodeId":null,"preferredNodeId":null}"""
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
            compose.waitUntil(20000) {compose.onAllNodesWithText("Node : Mon serveur",substring=true).fetchSemanticsNodes().isNotEmpty()}
            compose.onNodeWithText("Automatique").performClick()
            compose.onNodeWithText("Préférer une node").performClick()
            compose.onNodeWithText("Enregistrer la préférence").performScrollTo().performClick()
            compose.waitUntil(10000) {saved.size==1}
            assertEquals("node",saved[0]["preferredNodeId"]?.jsonPrimitive?.content)
            assertEquals(JsonNull,saved[0]["pinnedNodeId"])
            compose.onNodeWithText("Préférer une node").performScrollTo().performClick()
            compose.onNodeWithText("Fixer à une node").performClick()
            compose.onNodeWithText("Enregistrer la préférence").performScrollTo().performClick()
            compose.waitUntil(10000) {saved.size==2}
            assertEquals("node",saved[1]["pinnedNodeId"]?.jsonPrimitive?.content)
            assertEquals(JsonNull,saved[1]["preferredNodeId"])
            compose.onNodeWithText("Dernier état restaurable",substring=true).assertExists()
            compose.onNodeWithText("Préférence enregistrée",substring=true).assertExists()
            // Without a recorded provider session there is nothing to resume elsewhere yet.
            compose.onNodeWithText("Déplacer maintenant").performScrollTo().assertIsNotEnabled()
        }
    }

    @Test fun ownerCanEnrollConfigureAndRevokeANode() {
        MockWebServer().use { server ->
            val writes = CopyOnWriteArrayList<Pair<String, JsonObject>>()
            var revoked = false
            var configured = false
            var granted = false
            var cleaned = false
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
                        path == "/api/nodes/node" -> { configured = true; "{}" }
                        path == "/api/nodes/settings" -> "{}"
                        path == "/api/nodes/node/agents" -> { granted = true; "{}" }
                        path == "/api/nodes/node/stale-disks/delete" -> { cleaned = true; """{"freedMiB":2048,"failed":0}""" }
                        path == "/api/nodes" -> """[{"id":"node","name":"Serveur test","status":"${if (revoked) "revoked" else "online"}","revoked":$revoked,"accepting":true,"limits":{"cpu":4,"memoryMiB":8192,"diskMiB":65536},"agents":${if (granted) """[{"id":"agent","name":"Agent test"}]""" else "[]"},"staleDisks":{"count":${if (cleaned) 0 else 1},"diskMiB":2048}}]"""
                        path == "/api/agents" -> """[{"id":"agent","name":"Agent test"}]"""
                        path == "/api/session" -> """{"authenticated":true,"csrf":"fixture"}"""
                        path == "/api/overview" -> "{}"
                        else -> "[]"
                    }
                    return MockResponse().setBody(body).apply {
                        // Keep the save busy while the subsequent node refresh is in flight.
                        if (path == "/api/nodes" && configured) setBodyDelay(1, java.util.concurrent.TimeUnit.SECONDS)
                    }
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
            compose.waitUntil(20000) { compose.onAllNodesWithText("Serveur test").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Aucun agent ne peut encore utiliser cette machine.", substring = true).assertExists()
            compose.onNodeWithText("Choisir les agents").performScrollTo().performClick()
            compose.waitUntil(10000) { compose.onAllNodesWithText("Agent test").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Agent test").performClick()
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { writes.any { it.first == "/api/nodes/node/agents" } && compose.onAllNodesWithText("Utilisée par Agent test").fetchSemanticsNodes().isNotEmpty() }
            assertEquals("agent", writes.first { it.first == "/api/nodes/node/agents" }.second["agentIds"]!!.jsonArray.single().jsonPrimitive.content)
            compose.onNodeWithText("Anciens disques : 2 Gio (1 conversation)").assertExists()
            compose.onNodeWithText("Libérer").performScrollTo().performClick()
            compose.waitUntil(10000) { compose.onAllNodes(hasText("Confirmer") and isEnabled()).fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(10000) { writes.any { it.first == "/api/nodes/node/stale-disks/delete" } && compose.onAllNodesWithText("Anciens disques", substring = true).fetchSemanticsNodes().isEmpty() }
            compose.onNodeWithText("Nom de la machine").performTextInput("Nouvelle node")
            compose.onNodeWithText("Créer un code d’inscription").performScrollTo().performClick()
            compose.waitUntil(10000) { compose.onAllNodesWithText("fixture-enrollment").fetchSemanticsNodes().isNotEmpty() }
            assertEquals("Nouvelle node", writes.first { it.first == "/api/nodes/enrollments" }.second["name"]?.jsonPrimitive?.content)
            compose.onNodeWithText("Masquer").performScrollTo().performClick()
            compose.onNodeWithText("Avancé : sauvegardes et délais").performScrollTo().performClick()
            compose.onNodeWithText("Configurer les sauvegardes").performScrollTo().performClick()
            compose.waitUntil(10000) { compose.onAllNodesWithText("Intervalle (secondes)").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Intervalle (secondes)").performTextReplacement("30")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { writes.any { it.first == "/api/nodes/settings" } && compose.onAllNodesWithText("Sauvegardes de VM").fetchSemanticsNodes().isEmpty() }
            assertEquals(30, writes.first { it.first == "/api/nodes/settings" }.second["intervalSeconds"]!!.jsonPrimitive.int)
            compose.onNodeWithText("Configurer", substring = false).performScrollTo().performClick()
            compose.onNodeWithText("Plafond CPU").performTextReplacement("3")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { writes.any { it.first == "/api/nodes/node" } }
            assertEquals(3, writes.first { it.first == "/api/nodes/node" }.second["limits"]!!.jsonObject["cpu"]!!.jsonPrimitive.int)
            compose.onNodeWithText("Révoquer", substring = false).performScrollTo().performClick()
            compose.waitUntil(10000) { compose.onAllNodes(hasText("Confirmer") and isEnabled()).fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Confirmer").assertIsEnabled().performClick()
            compose.waitUntil(10000) { compose.onAllNodesWithText("Afficher les machines révoquées (1)").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Afficher les machines révoquées (1)").performScrollTo().performClick()
            compose.waitUntil(10000) { compose.onAllNodesWithText("Révoquée").fetchSemanticsNodes().isNotEmpty() }
            assertTrue(writes.any { it.first == "/api/nodes/node/revoke" })
        }
    }

}
