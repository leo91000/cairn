package build.cairn.app.ui

import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModel
import build.cairn.app.data.CairnApi
import build.cairn.app.data.MemoryVault
import java.io.IOException
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import okhttp3.Call
import okhttp3.EventListener
import okhttp3.OkHttpClient
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.SocketPolicy
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class FixtureUiTest {
    @get:Rule val compose = createComposeRule()

    @Test fun closeCancelsHttpBeforeClearingTheViewModelAndClosingTheServer() = pendingRead(false)

    @Test fun closeAlsoDisposesTheCompositionWhenTheMainClockIsPaused() = pendingRead(true)

    private fun pendingRead(pauseClock: Boolean) {
        val callCancelled = AtomicBoolean(false)
        val cancelledBeforeModelCleared = AtomicBoolean(false)
        val job = AtomicReference<Job?>()
        val failure = AtomicReference<IOException?>()
        val client =
            OkHttpClient.Builder()
                .eventListener(
                    object : EventListener() {
                        override fun canceled(call: Call) {
                            callCancelled.set(true)
                        }
                    }
                )
                .build()
        val model =
            object : ViewModel() {
                override fun onCleared() {
                    cancelledBeforeModelCleared.set(
                        job.get()?.isCancelled == true && callCancelled.get()
                    )
                }
            }

        MockWebServer().use { server ->
            server.enqueue(MockResponse().setSocketPolicy(SocketPolicy.NO_RESPONSE))
            server.start()
            val api = CairnApi(server.url("/"), MemoryVault(), client)
            try {
                FixtureUi(compose).use { ui ->
                    ui.setContent(model) {
                        LaunchedEffect(Unit) {
                            job.set(currentCoroutineContext()[Job])
                            try {
                                api.request("GET", "/held")
                            } catch (error: CancellationException) {
                                throw error
                            } catch (error: IOException) {
                                failure.set(error)
                            }
                        }
                    }
                    compose.waitUntil(5000) { server.requestCount == 1 }
                    assertTrue(job.get()!!.isActive)
                    if (pauseClock) compose.mainClock.autoAdvance = false
                }
                assertEquals(!pauseClock, compose.mainClock.autoAdvance)
                // The real HTTP call is cancelled while the server is still alive.
                assertTrue(
                    "Composition work must be cancelled before closing the server",
                    job.get()!!.isCancelled,
                )
                assertTrue(
                    "The real HTTP call must be cancelled before closing the server",
                    callCancelled.get(),
                )
                assertTrue(
                    "The composition and HTTP call must be cancelled before clearing the model",
                    cancelledBeforeModelCleared.get(),
                )
                assertNull(failure.get())
            } finally {
                compose.mainClock.autoAdvance = true
                client.dispatcher.cancelAll()
                client.connectionPool.evictAll()
                client.dispatcher.executorService.shutdown()
            }
        }
    }
}
