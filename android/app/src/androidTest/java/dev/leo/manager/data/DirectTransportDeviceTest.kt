package dev.leo.manager.data

import android.app.Application
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.os.Build
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.leo.manager.ui.LeoApp
import dev.leo.manager.ui.LeoTheme
import java.io.File
import java.util.concurrent.ConcurrentLinkedQueue
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.onEach
import kotlinx.serialization.json.*
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.OkHttpClient
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Real libwebrtc, control plane and installation; driven by the existing network bench. */
@RunWith(AndroidJUnit4::class)
class DirectTransportDeviceTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun authorizedInstallationCarriesAnObservedDirectResponse() = runBlocking {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        val scenario =
            InstrumentationRegistry.getArguments().getString("leoDirectScenario") ?: "direct-only"
        val configuration =
            wireJson
                .parseToJsonElement(File(context.filesDir, "direct-fixture.json").readText())
                .jsonObject
        val origin = configuration["origin"]!!.jsonPrimitive.content.toHttpUrl()
        val vault =
            object : SessionVault {
                override fun read(origin: String) =
                    configuration["cookie"]!!.jsonPrimitive.content + "; Path=/; HttpOnly"

                override fun write(origin: String, cookie: String?) {}
            }
        // Observe control acknowledgement without retaining SDP, IDs, headers or credentials.
        val control = ConcurrentLinkedQueue<String>()
        val client =
            OkHttpClient.Builder()
                .addNetworkInterceptor { chain ->
                    val response = chain.proceed(chain.request())
                    val path = chain.request().url.encodedPath
                    if (path.contains("/direct/"))
                        control.add(path.substringAfterLast('/') + ":" + response.code)
                    response
                }
                .build()
        val api =
            LeoApi(origin, vault, client, configuration["installation"]!!.jsonPrimitive.content)
        api.csrf = configuration["csrf"]!!.jsonPrimitive.content
        var directReads = 0
        var ui = false
        var renewed = false
        var revoked = false
        var networkChanges = 0
        var primaryRoute = "relay"
        val started = System.nanoTime()

        suspend fun read() {
            assertEquals("[]", api.request("GET", "/chats"))
            if (api.transport.route.value == "direct") directReads++
        }
        suspend fun awaitDirect() =
            withTimeout(60000) {
                do {
                    delay(200)
                    read()
                } while (api.transport.route.value != "direct")
            }
        fun shell(command: String) {
            instrumentation.uiAutomation.executeShellCommand(command).use { descriptor ->
                android.os.ParcelFileDescriptor.AutoCloseInputStream(descriptor).use {
                    it.readBytes()
                }
            }
        }
        try {
            read()
            assertEquals("relay", api.transport.route.value)
            api.startDirect(context)
            if (scenario == "udp-blocked") {
                // Let a full real ICE attempt expire while reads continue over HTTPS.
                val deadline = System.nanoTime() + java.util.concurrent.TimeUnit.SECONDS.toNanos(35)
                while (System.nanoTime() < deadline) {
                    delay(500)
                    read()
                    assertEquals("relay", api.transport.route.value)
                }
                assertTrue(control.contains("authorize:200"))
                assertTrue(control.contains("signal:204"))
                assertEquals(0, directReads)
            } else {
                awaitDirect()
                primaryRoute = api.transport.route.value
                assertTrue(control.contains("authorize:200"))
                assertTrue(control.contains("signal:204"))

                if (scenario == "network-change") {
                    val manager = context.getSystemService(ConnectivityManager::class.java)
                    shell("svc data enable")
                    for ((command, target) in
                        listOf(
                            "svc wifi disable" to NetworkCapabilities.TRANSPORT_CELLULAR,
                            "svc wifi enable" to NetworkCapabilities.TRANSPORT_WIFI,
                        )) {
                        val previous = manager.activeNetwork
                        assertNotNull(previous)
                        val fallback = CompletableDeferred<Unit>()
                        val observer =
                            launch(Dispatchers.Default) {
                                api.transport.route.collect {
                                    if (it == "relay") fallback.complete(Unit)
                                }
                            }
                        val authorizations = control.count { it == "authorize:200" }
                        try {
                            shell(command)
                            withTimeout(15000) {
                                while (
                                    manager.activeNetwork == previous ||
                                        manager
                                            .getNetworkCapabilities(manager.activeNetwork)
                                            ?.hasTransport(target) != true
                                ) delay(100)
                                fallback.await()
                            }
                            awaitDirect()
                            assertTrue(control.count { it == "authorize:200" } > authorizations)
                            networkChanges++
                        } finally {
                            observer.cancelAndJoin()
                        }
                    }
                    assertEquals(2, networkChanges)
                }

                if (scenario == "same-lan") {
                    // Real selected-installation UI exercises the asynchronous startup/indicator.
                    val vm =
                        withContext(Dispatchers.Main) {
                            LeoViewModel(
                                context.applicationContext as Application,
                                vault,
                                origin.toString(),
                            )
                        }
                    compose.setContent { LeoTheme { LeoApp(vm = vm) } }
                    compose.waitUntil(30000) {
                        compose
                            .onAllNodesWithContentDescription("Transport : Direct")
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    compose.onNodeWithContentDescription("Transport : Direct").assertIsDisplayed()
                    vm.api.closeStreams()
                    ui = true

                    // The delivered lease lasts 180 s: observe a real renewal before expiry.
                    withTimeout(180000) {
                        while (!control.contains("renew:200")) {
                            delay(1000)
                            read()
                            assertEquals("direct", api.transport.route.value)
                        }
                    }
                    renewed = true
                    val streaming = CompletableDeferred<Unit>()
                    val terminal = async {
                        api.live("/chats/stream")
                            .onEach { if (it.state?.chats != null) streaming.complete(Unit) }
                            .first { it.httpStatus == 401 || it.httpStatus == 403 }
                    }
                    withTimeout(15000) { streaming.await() }
                    assertEquals("direct", api.transport.route.value)
                    api.request("POST", "/account/logout")
                    val status = withTimeout(10000) { terminal.await().httpStatus }
                    assertEquals(401, status)
                    assertEquals("relay", api.transport.route.value)
                    val rejected = runCatching { api.request("GET", "/chats") }.exceptionOrNull()
                    assertTrue(rejected is ApiException && rejected.status == 401)
                    revoked = true
                }
            }
            File(context.filesDir, "direct-evidence.json")
                .writeText(
                    buildJsonObject {
                        put("scenario", scenario)
                        put("firstRoute", "relay")
                        put("route", primaryRoute)
                        put("finalRoute", api.transport.route.value)
                        put("directResponses", directReads)
                        put("indicatorObserved", ui)
                        put("renewalAcknowledged", renewed)
                        put("revokedDuringLive", revoked)
                        put("networkChanges", networkChanges)
                        put("sdk", Build.VERSION.SDK_INT)
                        put("abi", Build.SUPPORTED_ABIS.first())
                        put("elapsedMs", (System.nanoTime() - started) / 1000000)
                    }
                        .toString()
                )
        } finally {
            api.closeStreams()
            if (scenario == "network-change") shell("svc wifi enable")
        }
    }
}
