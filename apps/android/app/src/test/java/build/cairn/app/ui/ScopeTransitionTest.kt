package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.datastore.preferences.core.edit
import androidx.lifecycle.ViewModelStore
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import build.cairn.app.data.*
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit.SECONDS
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.*
import okhttp3.mockwebserver.*
import org.junit.*
import org.junit.Assert.*
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowLooper

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
class ScopeTransitionTest {
    @get:Rule val compose = createComposeRule()
    private val application = ApplicationProvider.getApplicationContext<Application>()
    private lateinit var jobs: CoroutineScope
    private val models = ViewModelStore()
    private val server = MockWebServer()
    private val values = ConcurrentHashMap<String, String>()
    private val vault =
        object : SessionVault {
            override fun read(origin: String): String? = values[origin]

            override fun write(origin: String, cookie: String?) {
                if (cookie == null) values.remove(origin) else values[origin] = cookie
            }
        }
    private val first = Installation("first", "Première", InstallationRole.Owner)
    private val second = Installation("second", "Deuxième", InstallationRole.Member)
    private val session =
        AtomicReference(
            Session(true, "csrf", CairnAccount("account", "a@example.test"), listOf(first, second))
        )
    private var status = 200
    @Volatile private var holdInstallations = false
    private val installationsReleased = CountDownLatch(1)
    private lateinit var vm: CairnViewModel
    private lateinit var cachedFile: File
    private var mounted by mutableStateOf(false)

    @Before
    fun setup() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            application,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        server.dispatcher =
            object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val path = request.path!!.substringBefore('?')
                    val result =
                        when {
                            path == "/api/account/session" ->
                                return MockResponse()
                                    .setResponseCode(status)
                                    .setBody(wireJson.encodeToString(session.get()))
                            path == "/api/installations" ->
                                wireJson.encodeToString(session.get().installations).also {
                                    if (holdInstallations) installationsReleased.await(30, SECONDS)
                                }
                            path == "/api/account/logout" ->
                                return MockResponse().setResponseCode(503).setBody("{}")
                            path == "/api/account/options" || path == "/api/account/methods" -> "{}"
                            path.endsWith("/overview") -> installationOverviewFixture()
                            path.endsWith("/models") -> "{}"
                            else ->
                                if (
                                    request.requestUrl?.encodedPath?.endsWith("/api/projects") ==
                                        true
                                )
                                    """{"projects":[]}"""
                                else if (
                                    request.requestUrl?.encodedPath?.endsWith("/api/agents") == true
                                )
                                    """{"agents":[]}"""
                                else if (
                                    request.requestUrl?.encodedPath?.endsWith("/api/tasks") == true
                                )
                                    """{"tasks":[]}"""
                                else if (
                                    request.requestUrl?.encodedPath?.endsWith("/api/mcps") == true
                                )
                                    """{"mcps":[]}"""
                                else "[]"
                        }
                    return MockResponse().setBody(result)
                }
            }
        server.start()
        vm = CairnViewModel(application, vault, beaconOrigin = "")
        models.put("fixture", vm)
        compose.setContent {
            jobs = rememberCoroutineScope()
            CairnTheme {
                if (mounted) CairnApp(vm = vm) else Box(Modifier.fillMaxSize())
            }
        }
        compose.waitUntil(20000) { vm.state.value.ready }
        action {
            application.dataStore.edit { it.clear() }
            values[server.url("/").toString()] = "cairn_session=fixture; path=/"
            vm.connect(server.url("/").toString())
        }
    }

    @After
    fun cleanup() {
        installationsReleased.countDown()
        if (::jobs.isInitialized) jobs.cancel()
        models.clear()
        server.close()
        WorkManagerTestInitHelper.closeWorkDatabase()
    }

    private fun start(block: suspend () -> Unit): Pair<Job, AtomicReference<Throwable?>> {
        val failure = AtomicReference<Throwable?>(null)
        lateinit var job: Job
        compose.runOnIdle {
            job = jobs.launch {
                try {
                    block()
                } catch (error: Throwable) {
                    failure.set(error)
                }
            }
        }
        return job to failure
    }

    private fun finish(pending: Pair<Job, AtomicReference<Throwable?>>) {
        compose.waitUntil(30000) {
            // Compose's clock does not deliver platform callbacks on Android's main Looper.
            ShadowLooper.idleMainLooper()
            pending.first.isCompleted
        }
        pending.second.get()?.let { throw AssertionError("Fixture action failed", it) }
        compose.waitForIdle()
    }

    private fun action(block: suspend () -> Unit) = finish(start(block))

    private fun seed(): Long {
        action {
            vm.historyCache.save(
                "retained",
                timelineCache(
                    1,
                    "v2:r:1",
                    LiveState(),
                    listOf(
                        RunEvent(
                            id = 1,
                            createdAt = 1,
                            type = "chat.user",
                            text = "fixture",
                        )
                    ),
                ),
                true,
            )
            vm.notifications.setSeen(setOf("question"))
            vm.notifications.setAlertsUntil(42)
        }
        compose.runOnIdle {
            vm.localReads["chat:read"] = 42
            vm.chatDrafts["draft"] = ChatDraft(text = "private draft")
            cachedFile =
                File(application.cacheDir, "cairn-files/retained").apply {
                    parentFile!!.mkdirs()
                    writeText("fixture")
                }
        }
        return vm.historyCache.generation
    }

    private fun assertTransientCleared() {
        compose.runOnIdle {
            assertTrue(vm.localReads.isEmpty())
            assertTrue(vm.chatDrafts.isEmpty())
        }
    }

    private fun assertStorageCleared(generation: Long) {
        assertTrue(vm.historyCache.generation > generation)
        assertFalse(cachedFile.exists())
        action { assertNull(vm.historyCache.read("retained")) }
        assertTransientCleared()
    }

    @Test
    fun sameScopeAndCsrfRetainStorageAndNotificationHistory() {
        val generation = seed()
        action { vm.retrySession() }
        assertEquals(generation, vm.historyCache.generation)
        assertTrue(cachedFile.exists())
        action {
            assertNotNull(vm.historyCache.read("retained"))
            assertEquals(setOf("question"), vm.notifications.seen())
            assertEquals(42L, vm.notifications.alertsUntil())
        }
        assertTransientCleared()
    }

    @Test
    fun csrfAccountInstallationAndRoleEachInvalidateStorage() {
        val changes =
            listOf<(Session) -> Session>(
                { it.copy(csrf = "new-csrf") },
                { it.copy(account = CairnAccount("other", "b@example.test")) },
                { it.copy(installations = listOf(second)) },
                { it.copy(installations = listOf(second.copy(role = InstallationRole.Owner))) },
            )
        for (change in changes) {
            val generation = seed()
            session.set(change(session.get()))
            action { vm.retrySession() }
            assertStorageCleared(generation)
        }
    }

    @Test
    fun revokedLastAccessClearsStorageAndLeavesAccountSignedIn() {
        val generation = seed()
        session.set(session.get().copy(installations = emptyList()))
        compose.runOnIdle { vm.report(ApiException(403, "access revoked")) }
        compose.waitUntil(20000) {
            ShadowLooper.idleMainLooper()
            vm.state.value.installation == null
        }
        assertStorageCleared(generation)
        assertTrue(vm.state.value.session.authenticated)
        assertNull(vm.state.value.installation)
        action {
            assertTrue(vm.notifications.seen().isEmpty())
            assertNull(vm.notifications.alertsUntil())
        }
    }

    @Test
    fun revokedLastAccessExplainsTheChangeOnTheAccountPage() {
        session.set(session.get().copy(installations = emptyList()))
        compose.runOnIdle { vm.report(ApiException(403, "access revoked")) }
        compose.waitUntil(20000) {
            ShadowLooper.idleMainLooper()
            vm.state.value.notice != null && !vm.state.value.busy
        }
        assertNull(vm.state.value.installation)

        // Hold the page's first installation poll: its refresh would replace the account state
        // before the notice could be inspected.
        holdInstallations = true
        compose.runOnIdle { mounted = true }
        val notice = compose.onNodeWithText("Votre accès à cette installation a changé.")
        compose.waitUntil(20000) {
            ShadowLooper.idleMainLooper()
            runCatching { notice.assertIsDisplayed() }.isSuccess
        }
        compose.onNodeWithText("Aucune installation").assertIsDisplayed()
    }

    @Test
    fun absentSessionClearsLocalStateWithApplicationMounted() {
        compose.runOnIdle { mounted = true }
        val generation = seed()
        session.set(Session())
        action { vm.retrySession() }
        assertStorageCleared(generation)
        assertFalse(vm.state.value.session.authenticated)
        assertTrue(values.isEmpty())
    }

    @Test
    fun expiredSessionImmediatelyLeavesWorkspaceAndEventuallyForgetsCredentials() {
        val generation = seed()
        compose.runOnIdle {
            vm.report(ApiException(401, "expired"))
            assertFalse(vm.state.value.session.authenticated)
            assertEquals("Session expirée. Reconnectez-vous.", vm.state.value.error)
            assertTrue(values.isEmpty())
        }
        compose.waitUntil(20000) {
            ShadowLooper.idleMainLooper()
            vm.historyCache.generation > generation && !cachedFile.exists() && values.isEmpty()
        }
        assertStorageCleared(generation)
    }

    @Test
    fun refusedLogoutStillForgetsSessionAndClearsPrivateState() {
        val generation = seed()
        action {
            try {
                vm.logout()
                fail("Fixture refuses logout")
            } catch (error: ApiException) {
                assertEquals(503, error.status)
            }
        }
        assertStorageCleared(generation)
        assertFalse(vm.state.value.session.authenticated)
        assertTrue(values.isEmpty())
    }

    @Test
    fun beaconFailurePreservesRestorableSessionAndCachedFiles() {
        val generation = seed()
        values[server.url("/").toString()] = "cairn_session=fixture; path=/"
        val before = values.toMap()
        status = 503
        action { vm.retrySession() }
        assertTrue(vm.state.value.restoringSession)
        assertEquals(before, values)
        assertEquals(generation, vm.historyCache.generation)
        assertTrue(cachedFile.exists())
    }

    @Test
    fun mountedWorkspaceKeepsItsApiWhileSwitchAndLastAccessCleanupSuspend() {
        compose.runOnIdle { mounted = true }
        suspendCleanup { vm.selectInstallation(second.id) }
        assertEquals(second.id, vm.state.value.installation?.id)
        session.set(session.get().copy(installations = emptyList()))
        suspendCleanup { vm.refreshInstallations() }
        assertNull(vm.state.value.installation)
        assertTrue(vm.state.value.session.authenticated)
    }

    private fun suspendCleanup(block: suspend () -> Unit) {
        val entered = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        val lock = start {
            application.dataStore.updateData {
                entered.complete(Unit)
                release.await()
                it
            }
        }
        compose.waitUntil(20000) { entered.isCompleted }
        val oldApi = vm.api
        val pending = start(block)
        try {
            compose.waitForIdle()
            compose.runOnIdle {
                assertFalse(pending.first.isCompleted)
                assertNotNull(vm.state.value.installation)
                assertSame(oldApi, vm.api)
            }
        } finally {
            release.complete(Unit)
        }
        finish(lock)
        finish(pending)
    }
}
