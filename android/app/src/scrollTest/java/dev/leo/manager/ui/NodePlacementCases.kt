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
}
