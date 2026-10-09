package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import build.cairn.app.data.*
import kotlinx.coroutines.runBlocking
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class CairnAccountRestorationDeviceTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun restoringTheAccountKeepsEncryptedHistoryAndReadingPosition() {
        MockWebServer().use { server ->
            val installations =
                """[{"id":"cached-installation","name":"Cache","role":"member","online":false}]"""
            val session =
                """{"authenticated":true,"csrf":"restore-csrf","account":{"id":"restore-person","email":"reader@example.test"},"installations":$installations}"""
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse =
                        when (request.path) {
                            "/api/account/session" -> MockResponse().setBody(session)
                            "/api/installations" -> MockResponse().setBody(installations)
                            else -> MockResponse().setResponseCode(404)
                        }
                }
            server.start()
            val application = ApplicationProvider.getApplicationContext<Application>()
            val origin = server.url("/").toString()
            val vault = KeystoreSessionVault(application)
            vault.write(origin, "cairn_session=restore-fixture; Path=/; Max-Age=3600; HttpOnly")
            val model = mutableStateOf(CairnViewModel(application, vault, beaconOrigin = origin))
            compose.setContent {
                val vm = model.value
                key(vm) { CairnTheme { CairnApp(vm = vm) } }
            }
            fun awaitRestoredInstallation() {
                compose.waitUntil(30000) {
                    model.value.state.value.installation?.id == "cached-installation" &&
                        !model.value.state.value.busy
                }
                compose.onNodeWithText("Cache · Hors ligne").assertIsDisplayed()
            }
            awaitRestoredInstallation()
            val original = model.value
            val cacheKey =
                original.historyCache.key(
                    original.api.cacheScope,
                    original.api.csrf,
                    "/chats/cached/stream",
                )
            val position = ReadingPosition(0, 12, false, 7)
            runBlocking {
                original.historyCache.save(
                    cacheKey,
                    CachedHistory(
                        7,
                        "v1:restore:1",
                        LiveState(),
                        listOf(RunEvent(7, 1, "chat.user", "Message conservé")),
                        position = position,
                    ),
                    force = true,
                )
                assertEquals(
                    "Message conservé",
                    original.historyCache.read(cacheKey)?.events?.single()?.text,
                )
            }
            compose.runOnIdle {
                model.value = CairnViewModel(application, vault, beaconOrigin = origin)
            }
            awaitRestoredInstallation()
            val restored = runBlocking { model.value.historyCache.read(cacheKey) }
            assertEquals("Message conservé", restored?.events?.single()?.text)
            assertEquals(position, restored?.position)
        }
    }
}
