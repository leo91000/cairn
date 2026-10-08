package dev.leo.manager.data

import android.app.Activity
import android.app.Application
import android.content.Intent
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.os.Build
import androidx.compose.runtime.*
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.leo.manager.ui.LeoApp
import dev.leo.manager.ui.LeoTheme
import java.io.File
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.onEach
import kotlinx.serialization.json.*
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
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
        val initialExpiry = AtomicLong()
        val client =
            OkHttpClient.Builder()
                .addNetworkInterceptor { chain ->
                    val response = chain.proceed(chain.request())
                    val path = chain.request().url.encodedPath
                    if (path.contains("/direct/"))
                        control.add(path.substringAfterLast('/') + ":" + response.code)
                    if (
                        scenario == "same-lan" &&
                            path.endsWith("/authorize") &&
                            response.isSuccessful
                    ) {
                        val grant =
                            wireJson
                                .parseToJsonElement(response.peekBody(65536).string())
                                .jsonObject
                        initialExpiry.set(
                            grant["grant"]!!
                                .jsonObject["claims"]!!
                                .jsonObject["expires_at"]!!
                                .jsonPrimitive
                                .long
                        )
                    }
                    if (
                        scenario == "same-lan" && path.endsWith("/renew") && response.isSuccessful
                    ) {
                        // Model a session-capped renewal through the HTTP seam. The actual
                        // installation
                        // still validates the real signed renewal; only this client's scheduling
                        // input is capped.
                        val grant = wireJson.parseToJsonElement(response.body.string()).jsonObject
                        val signed = grant["grant"]!!.jsonObject
                        val claims = signed["claims"]!!.jsonObject
                        val capped =
                            JsonObject(
                                grant +
                                    ("grant" to
                                        JsonObject(
                                            signed +
                                                ("claims" to
                                                    JsonObject(
                                                        claims +
                                                            ("expires_at" to
                                                                JsonPrimitive(initialExpiry.get()))
                                                    ))
                                        ))
                            )
                        response
                            .newBuilder()
                            .body(capped.toString().toResponseBody(response.body.contentType()))
                            .build()
                    } else response
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
        var backgroundStopped = false
        var suspendedControl = false
        var cappedRenewal = false
        val started = System.nanoTime()
        lateinit var activity: Activity
        var selectedVm by mutableStateOf<LeoViewModel?>(null)
        compose.setContent {
            activity = LocalContext.current as Activity
            selectedVm?.let { vm -> LeoTheme { LeoApp(vm = vm) } }
        }

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
                    val authorizations = control.count { it == "authorize:200" }
                    val inFlight =
                        List(8) {
                            async(Dispatchers.IO) {
                                repeat(8) { assertEquals("[]", api.request("GET", "/chats")) }
                            }
                        }
                    instrumentation.runOnMainSync { assertTrue(activity.moveTaskToBack(true)) }
                    withTimeout(5000) {
                        while (api.transport.route.value != "relay") delay(100)
                    }
                    delay(500)
                    val stoppedControlCount = control.size
                    delay(2000)
                    assertEquals(
                        "No signaling while the app is stopped",
                        stoppedControlCount,
                        control.size,
                    )
                    inFlight.awaitAll()
                    backgroundStopped = true
                    instrumentation.runOnMainSync {
                        activity.startActivity(
                            Intent(activity, activity.javaClass)
                                .addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT)
                        )
                    }
                    awaitDirect()
                    assertTrue(control.count { it == "authorize:200" } > authorizations)

                    for (status in listOf(200, 401, 403)) {
                        val attempts = AtomicInteger()
                        val unavailableClient =
                            client
                                .newBuilder()
                                .addInterceptor { chain ->
                                    if (
                                        chain
                                            .request()
                                            .url
                                            .encodedPath
                                            .endsWith("/direct/authorize")
                                    ) {
                                        attempts.incrementAndGet()
                                        Response.Builder()
                                            .request(chain.request())
                                            .protocol(Protocol.HTTP_1_1)
                                            .code(status)
                                            .message("Control fixture")
                                            .body("{\"available\":false}".toResponseBody())
                                            .build()
                                    } else chain.proceed(chain.request())
                                }
                                .build()
                        val probe = LeoApi(origin, vault, unavailableClient, api.installationId)
                        probe.csrf = api.csrf
                        try {
                            probe.startDirect(context)
                            withTimeout(10000) { while (attempts.get() == 0) delay(100) }
                            delay(18000)
                            assertEquals(
                                "Control rejection must suspend reconnects: $status",
                                1,
                                attempts.get(),
                            )
                            assertEquals("[]", probe.request("GET", "/chats"))
                            assertEquals("relay", probe.transport.route.value)
                        } finally {
                            probe.closeStreams()
                        }
                    }
                    suspendedControl = true
                    val largeRequest = runCatching {
                        api.request(
                            "POST",
                            "/chats/missing/messages",
                            body(
                                "id" to java.util.UUID.randomUUID().toString(),
                                "text" to "x".repeat(1_000_000),
                            ),
                        )
                    }
                        .exceptionOrNull()
                    assertEquals(
                        "The real router retains its 150 kB JSON body limit",
                        413,
                        (largeRequest as? ApiException)?.status,
                    )
                    assertEquals("direct", api.transport.route.value)
                    read()
                    assertEquals("direct", api.transport.route.value)

                    // Real selected-installation UI exercises the asynchronous startup/indicator.
                    val vm =
                        withContext(Dispatchers.Main) {
                            LeoViewModel(
                                context.applicationContext as Application,
                                vault,
                                origin.toString(),
                            )
                        }
                    withContext(Dispatchers.Main) { selectedVm = vm }
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
                    val renewals = control.count { it == "renew:200" }
                    delay(2000)
                    assertEquals(
                        "A capped deadline must not start a renewal loop",
                        renewals,
                        control.count { it == "renew:200" },
                    )
                    read()
                    assertEquals("direct", api.transport.route.value)
                    cappedRenewal = true
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
                        put("backgroundStopped", backgroundStopped)
                        put("controlSuspended", suspendedControl)
                        put("cappedRenewal", cappedRenewal)
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
