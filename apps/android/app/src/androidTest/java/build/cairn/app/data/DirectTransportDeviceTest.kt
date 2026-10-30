package build.cairn.app.data

import android.app.Activity
import android.app.Application
import android.content.Intent
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.os.Build
import androidx.activity.compose.LocalActivity
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import build.cairn.app.ui.CairnApp
import build.cairn.app.ui.CairnTheme
import build.cairn.app.ui.installationHeader
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.atomic.AtomicBoolean
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
import okio.Buffer
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Real libwebrtc, control plane and installation; driven by the existing network bench. */
@RunWith(AndroidJUnit4::class)
class DirectTransportDeviceTest {
    @get:Rule val compose = createComposeRule()
    private val diagnostics = DirectBenchDiagnostics()

    private suspend fun <T> duringPhase(
        name: String,
        timeoutMillis: Long,
        block: suspend CoroutineScope.() -> T,
    ): T =
        try {
            diagnostics.record("phase start", name)
            withTimeout(timeoutMillis, block).also { diagnostics.record("phase completed", name) }
        } catch (error: TimeoutCancellationException) {
            throw AssertionError("Direct bench timed out during $name", error)
        }

    @Test
    fun authorizedInstallationCarriesAnObservedDirectResponse() = runBlocking {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        val scenario =
            InstrumentationRegistry.getArguments().getString("cairnDirectScenario") ?: "direct-only"
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
        val refusedPeers = ConcurrentHashMap.newKeySet<String>()
        val refusedCandidates = AtomicInteger()
        val authorizationAttempts = AtomicInteger()
        val client =
            OkHttpClient.Builder()
                .addInterceptor { chain ->
                    diagnostics.primaryRequests.incrementAndGet()
                    try {
                        chain.proceed(chain.request()).also { response ->
                            diagnostics.primaryResponses.incrementAndGet()
                            if (response.request.tag(DirectChannel::class.java) != null)
                                diagnostics.primaryDirectResponses.incrementAndGet()
                        }
                    } catch (error: Throwable) {
                        diagnostics.primaryFailures.incrementAndGet()
                        diagnostics.record("primary failure", error.javaClass.simpleName)
                        throw error
                    }
                }
                .addInterceptor { chain ->
                    val request = chain.request()
                    if (request.url.encodedPath.endsWith("/direct/authorize"))
                        authorizationAttempts.incrementAndGet()
                    if (scenario == "same-lan" && request.url.encodedPath.endsWith("/signal")) {
                        val body = Buffer().also { request.body!!.writeTo(it) }.readUtf8()
                        val signal = wireJson.parseToJsonElement(body).jsonObject
                        if (
                            signal["kind"]!!.jsonPrimitive.content == "candidate" &&
                                signal["candidate"]!!.jsonPrimitive.content.contains(" typ host") &&
                                refusedPeers.add(request.url.encodedPath)
                        ) {
                            // Refuse one trickled candidate per real peer, through the control
                            // HTTP seam. The remaining real STUN candidates must still connect.
                            val count = refusedCandidates.incrementAndGet()
                            return@addInterceptor Response.Builder()
                                .request(request)
                                .protocol(Protocol.HTTP_1_1)
                                .code(if (count % 2 == 1) 400 else 429)
                                .message("Candidate refused")
                                .body("{}".toResponseBody())
                                .build()
                        }
                    }
                    chain.proceed(request)
                }
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
            CairnApi(origin, vault, client, configuration["installation"]!!.jsonPrimitive.content)
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
        var foregroundBackoff = false
        var unavailableRenewal = false
        val started = System.nanoTime()
        lateinit var activity: Activity
        var selectedVm by mutableStateOf<CairnViewModel?>(null)
        val expectedInstallation = configuration["installation"]!!.jsonPrimitive.content
        fun recordWorkspace() =
            diagnostics.workspace(
                selectedVm,
                expectedInstallation,
                runCatching { activity }.getOrNull(),
            )
        fun headerCategory(): String {
            val headers = compose.onAllNodes(installationHeader()).fetchSemanticsNodes()
            if (headers.isEmpty()) return "absent"
            for ((label, category) in
                listOf(
                    "En ligne · Direct" to "direct",
                    "En ligne · Relais" to "relay",
                    "Hors ligne" to "offline",
                    "Mise à jour requise" to "update-required",
                )) {
                if (headers.any { installationHeader(state = label).matches(it) }) return category
            }
            return "other"
        }
        compose.setContent {
            activity = checkNotNull(LocalActivity.current)
            selectedVm?.let { vm -> CairnTheme { CairnApp(vm = vm) } }
        }

        suspend fun read(client: CairnApi = api) {
            assertEquals("[]", client.request("GET", "/chats"))
            if (client === api && client.transport.route.value == "direct") directReads++
        }
        suspend fun awaitDirect(
            client: CairnApi = api,
            phase: String = "initial direct response",
        ) =
            duringPhase(phase, 60000) {
                awaitDirectBenchRead(client) { read(client) }
            }
        fun shell(command: String) {
            instrumentation.uiAutomation.executeShellCommand(command).use { descriptor ->
                android.os.ParcelFileDescriptor.AutoCloseInputStream(descriptor).use {
                    it.readBytes()
                }
            }
        }
        try {
            diagnostics.record("startup", "primary read")
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
                if (scenario == "same-lan") assertTrue(refusedCandidates.get() > 0)
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
                            duringPhase("network transition", 15000) {
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
                    duringPhase("background transport stop", 5000) {
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

                    // A real foreground transition must retain the capacity-error deadline.
                    val capacityAttempts = AtomicInteger()
                    val rejectedAt = AtomicLong()
                    val retryAt = AtomicLong()
                    val capacityClient =
                        OkHttpClient.Builder()
                            .addInterceptor { chain ->
                                val request = chain.request()
                                if (request.url.encodedPath.endsWith("/direct/authorize")) {
                                    if (capacityAttempts.incrementAndGet() == 1) {
                                        rejectedAt.set(System.nanoTime())
                                        return@addInterceptor Response.Builder()
                                            .request(request)
                                            .protocol(Protocol.HTTP_1_1)
                                            .code(503)
                                            .message("Capacity fixture")
                                            .body("{}".toResponseBody())
                                            .build()
                                    }
                                    retryAt.set(System.nanoTime())
                                }
                                chain.proceed(request)
                            }
                            .build()
                    val capacity = CairnApi(origin, vault, capacityClient, api.installationId)
                    capacity.csrf = api.csrf
                    try {
                        capacity.startDirect(context)
                        duringPhase("capacity initial authorization", 10000) {
                            while (capacityAttempts.get() == 0) delay(100)
                        }
                        delay(500)
                        instrumentation.runOnMainSync { assertTrue(activity.moveTaskToBack(true)) }
                        duringPhase("background transport stop", 5000) {
                            while (api.transport.route.value != "relay") delay(100)
                        }
                        delay(1000)
                        instrumentation.runOnMainSync {
                            activity.startActivity(
                                Intent(activity, activity.javaClass)
                                    .addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT)
                            )
                        }
                        delay(2000)
                        assertEquals(
                            "Foreground must not bypass capacity backoff",
                            1,
                            capacityAttempts.get(),
                        )
                        duringPhase("capacity retry direct response", 25000) {
                            awaitDirectBenchRead(capacity) {
                                assertEquals("[]", capacity.request("GET", "/chats"))
                            }
                        }
                        assertTrue(
                            "Retry must retain its fifteen-second deadline",
                            retryAt.get() - rejectedAt.get() >= 15_000_000_000L,
                        )
                        foregroundBackoff = true
                    } finally {
                        capacity.closeStreams()
                    }
                    awaitDirect()

                    // Keep authorization/DTLS real. Shorten only the client's scheduling input
                    // so a real acknowledged renewal can return the unavailable control fixture.
                    val renewalAttempts = AtomicInteger()
                    val renewalReplies = AtomicInteger()
                    val attemptsWhenUnavailable = AtomicInteger(-1)
                    val renewalSetupRejected = AtomicBoolean()
                    val renewalTrace = ConcurrentLinkedQueue<String>()
                    val renewalStarted = System.nanoTime()
                    fun recordRenewal(event: String) {
                        val elapsedMillis = (System.nanoTime() - renewalStarted) / 1_000_000
                        renewalTrace.add("$elapsedMillis:$event")
                    }
                    val renewalClient =
                        OkHttpClient.Builder()
                            .addInterceptor { chain ->
                                val request = chain.request()
                                val path = request.url.encodedPath
                                if (path.endsWith("/direct/authorize")) {
                                    val attempt = renewalAttempts.incrementAndGet()
                                    recordRenewal("authorize:$attempt:start")
                                    if (attempt == 1) {
                                        // A valid setup retry must not count as a retry after the
                                        // later unavailable renewal response.
                                        renewalSetupRejected.set(true)
                                        recordRenewal("authorize:$attempt:503")
                                        return@addInterceptor Response.Builder()
                                            .request(request)
                                            .protocol(Protocol.HTTP_1_1)
                                            .code(503)
                                            .message("Renewal setup capacity fixture")
                                            .body("{}".toResponseBody())
                                            .build()
                                    }
                                }
                                val response =
                                    try {
                                        chain.proceed(request)
                                    } catch (error: java.io.IOException) {
                                        if (path.contains("/direct/"))
                                            recordRenewal(
                                                "${path.substringAfterLast('/')}:${error.javaClass.simpleName}"
                                            )
                                        throw error
                                    }
                                if (
                                    path.endsWith("/signal") ||
                                        path.endsWith("/events") ||
                                        path.endsWith("/renew")
                                )
                                    recordRenewal(
                                        "${path.substringAfterLast('/')}:${response.code}"
                                    )
                                if (path.endsWith("/direct/authorize"))
                                    recordRenewal("authorize:response:${response.code}")
                                if (path.endsWith("/direct/authorize") && response.isSuccessful) {
                                    // The control call permits 30 s. Exercise successful setup
                                    // beyond the former 10 s fixture precondition before testing
                                    // the unavailable renewal.
                                    Thread.sleep(12000)
                                    val grant =
                                        wireJson
                                            .parseToJsonElement(response.body.string())
                                            .jsonObject
                                    val signed = grant["grant"]!!.jsonObject
                                    val claims = signed["claims"]!!.jsonObject
                                    val scheduling =
                                        JsonObject(
                                            grant +
                                                ("grant" to
                                                    JsonObject(
                                                        signed +
                                                            ("claims" to
                                                                JsonObject(
                                                                    claims +
                                                                        ("expires_at" to
                                                                            JsonPrimitive(
                                                                                System
                                                                                    .currentTimeMillis() /
                                                                                    1000 + 40
                                                                            ))
                                                                ))
                                                    ))
                                        )
                                    response
                                        .newBuilder()
                                        .body(
                                            scheduling
                                                .toString()
                                                .toResponseBody(response.body.contentType())
                                        )
                                        .build()
                                } else if (path.endsWith("/renew") && response.isSuccessful) {
                                    // Capture the first real response boundary, before the client
                                    // can react. A later reply must never move this baseline.
                                    attemptsWhenUnavailable.compareAndSet(-1, renewalAttempts.get())
                                    recordRenewal(
                                        "renew:unavailable:authorizations=${renewalAttempts.get()}"
                                    )
                                    response.close()
                                    renewalReplies.incrementAndGet()
                                    response
                                        .newBuilder()
                                        .body("{\"available\":false}".toResponseBody())
                                        .build()
                                } else response
                            }
                            .build()
                    val renewal = CairnApi(origin, vault, renewalClient, api.installationId)
                    renewal.csrf = api.csrf
                    try {
                        renewal.startDirect(context)
                        awaitDirect(renewal, "unavailable renewal initial direct response")
                        recordRenewal("observed-direct")
                        duringPhase("unavailable renewal acknowledgement", 20000) {
                            while (
                                renewalReplies.get() == 0 ||
                                    renewal.transport.route.value != "relay"
                            ) delay(100)
                        }
                        assertTrue(
                            "Setup must retry the injected capacity rejection before renewal: $renewalTrace",
                            renewalSetupRejected.get() && attemptsWhenUnavailable.get() >= 2,
                        )
                        delay(18000)
                        assertEquals("[]", renewal.request("GET", "/chats"))
                        assertEquals("relay", renewal.transport.route.value)
                        assertEquals(
                            "Unavailable renewal must not authorize again: $renewalTrace",
                            attemptsWhenUnavailable.get(),
                            renewalAttempts.get(),
                        )
                        assertEquals(
                            "Unavailable renewal must not loop: $renewalTrace",
                            1,
                            renewalReplies.get(),
                        )
                        unavailableRenewal = true
                    } catch (error: CancellationException) {
                        throw error
                    } catch (error: AssertionError) {
                        throw AssertionError(
                            "${error.message}; renewal events=$renewalTrace",
                            error,
                        )
                    } catch (error: Exception) {
                        val failure =
                            if (error is ApiException) "HTTP ${error.status}"
                            else error.javaClass.simpleName
                        throw AssertionError(
                            "Renewal fixture failed ($failure); renewal events=$renewalTrace",
                            error,
                        )
                    } finally {
                        renewal.closeStreams()
                    }

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
                        val probe = CairnApi(origin, vault, unavailableClient, api.installationId)
                        probe.csrf = api.csrf
                        try {
                            probe.startDirect(context)
                            duringPhase("control rejection initial authorization: $status", 10000) {
                                while (attempts.get() == 0) delay(100)
                            }
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

                    awaitDirect()
                    val authorizationsBeforeLargeRequest = authorizationAttempts.get()
                    val largeRequest =
                        try {
                            api.request(
                                "POST",
                                "/chats/missing/messages",
                                body(
                                    "id" to java.util.UUID.randomUUID().toString(),
                                    "text" to "x".repeat(1_000_000),
                                ),
                            )
                            throw AssertionError("Expected the router's oversized-body rejection")
                        } catch (error: ApiException) {
                            error
                        }
                    assertEquals(
                        "The real router retains its 150 kB JSON body limit",
                        413,
                        largeRequest.status,
                    )
                    // An ID-backed POST may safely replay on relay under write pressure.
                    // The route records that response; observe a fresh direct read before
                    // asserting recovery, rather than requiring the rejection to stay direct.
                    awaitDirect()
                    assertEquals("direct", api.transport.route.value)
                    assertEquals(
                        "The rejected message must preserve the authorized peer",
                        authorizationsBeforeLargeRequest,
                        authorizationAttempts.get(),
                    )

                    // Real selected-installation UI exercises the asynchronous startup/indicator.
                    diagnostics.record("startup", "construct ViewModel")
                    val vm =
                        withContext(Dispatchers.Main) {
                            CairnViewModel(
                                context.applicationContext as Application,
                                vault,
                                origin.toString(),
                            )
                        }
                    withContext(Dispatchers.Main) { selectedVm = vm }
                    diagnostics.record("startup", "show ViewModel")
                    // Startup reads finish on relay before startDirect(). Peer readiness does
                    // not change the observed route: drive a real workspace read through this
                    // selected VM rather than relying on incidental live/poll traffic.
                    duringPhase("selected-installation direct UI", 30000) {
                        var refreshes = 0
                        var observedDirect = false
                        while (true) {
                            recordWorkspace()
                            val state = vm.state.value
                            if (
                                state.ready &&
                                    !state.busy &&
                                    state.session.authenticated &&
                                    state.installation?.id == expectedInstallation &&
                                    state.installation.online
                            ) {
                                diagnostics.record("selected VM refresh", "started ${++refreshes}")
                                withContext(Dispatchers.Main) { vm.refresh() }
                                diagnostics.record("selected VM refresh", "completed $refreshes")
                                observedDirect = vm.api.transport.route.value == "direct"
                                diagnostics.record(
                                    "selected VM response route",
                                    if (observedDirect) "direct" else "relay",
                                )
                            }
                            recordWorkspace()
                            val header = headerCategory()
                            diagnostics.record("header", header)
                            if (observedDirect && header == "direct") break
                            delay(200)
                        }
                    }
                    assertEquals(expectedInstallation, vm.state.value.installation?.id)
                    assertEquals("direct", vm.api.transport.route.value)
                    compose
                        .onNode(installationHeader(state = "En ligne · Direct"))
                        .assertIsDisplayed()
                    diagnostics.record("startup", "real Direct header displayed")
                    vm.api.closeStreams()
                    ui = true

                    // The delivered lease lasts 180 s: observe a real renewal before expiry.
                    duringPhase("real renewal", 180000) {
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
                    duringPhase("live stream startup", 15000) { streaming.await() }
                    assertEquals("direct", api.transport.route.value)
                    api.request("POST", "/account/logout")
                    val status =
                        duringPhase("live revocation", 10000) { terminal.await().httpStatus }
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
                        put("foregroundBackoff", foregroundBackoff)
                        put("unavailableRenewal", unavailableRenewal)
                        put("refusedCandidates", refusedCandidates.get())
                        put("phaseEvidence", diagnostics.summary())
                        put("sdk", Build.VERSION.SDK_INT)
                        put("abi", Build.SUPPORTED_ABIS.first())
                        put("elapsedMs", (System.nanoTime() - started) / 1000000)
                    }
                        .toString()
                )
        } catch (error: Throwable) {
            runCatching {
                recordWorkspace()
                diagnostics.record("header", headerCategory())
            }
                .onFailure { diagnostics.record("snapshot failure", it.javaClass.simpleName) }
            error.addSuppressed(AssertionError(diagnostics.summary()))
            throw error
        } finally {
            api.closeStreams()
            if (scenario == "network-change") shell("svc wifi enable")
        }
    }
}
