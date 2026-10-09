package build.cairn.app.data

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [32])
class NativeDeviceTest {
    @Before
    fun initializeWork() {
        androidx.work.testing.WorkManagerTestInitHelper.initializeTestWorkManager(
            ApplicationProvider.getApplicationContext(),
            androidx.work.Configuration.Builder()
                .setExecutor(androidx.work.testing.SynchronousExecutor())
                .build(),
        )
    }

    @Test
    fun `device registers once for all installations rotates its token and unregisters`() =
        runBlocking {
            val context = ApplicationProvider.getApplicationContext<Context>()
            val registrations = mutableListOf<Map<String, String>>()
            val removed = mutableListOf<String>()
            val account = UUID.randomUUID().toString()
            var token = "first-token"
            val registrationStatus = AtomicInteger(200)
            val delayedFailureStarted = CountDownLatch(1)
            val releaseDelayedFailure = CountDownLatch(1)
            val tokens =
                object : PushTokens {
                    override val available = true

                    override suspend fun token() = token

                    override suspend fun delete() {
                        token = "deleted"
                    }
                }
            MockWebServer().use { server ->
                server.dispatcher =
                    object : Dispatcher() {
                        override fun dispatch(request: RecordedRequest): MockResponse =
                            when {
                                request.path == "/api/account/session" ->
                                    MockResponse()
                                        .setBody(
                                            """{"authenticated":true,"csrf":"csrf","account":{"id":"$account","email":"a@example.test"},"installations":[]}"""
                                        )
                                request.path ==
                                    "/api/account/notifications/subscriptions/registered-device" &&
                                    request.method == "GET" ->
                                    MockResponse().setBody("""{"registered":true}""")
                                request.path == "/api/account/notifications/android" &&
                                    request.method == "GET" ->
                                    MockResponse().setBody("""{"enabled":true}""")
                                request.path == "/api/account/notifications/android" &&
                                    request.method == "POST" -> {
                                    assertEquals("csrf", request.getHeader("X-CSRF-Token"))
                                    val registration =
                                        wireJson
                                            .parseToJsonElement(request.body.readUtf8())
                                            .jsonObject
                                            .mapValues { it.value.jsonPrimitive.content }
                                    registrations += registration
                                    if (registration["token"] == "stale-token") {
                                        delayedFailureStarted.countDown()
                                        check(releaseDelayedFailure.await(10, TimeUnit.SECONDS))
                                        MockResponse()
                                            .setResponseCode(403)
                                            .setBody("""{"error":"Old registration refused"}""")
                                    } else if (registrationStatus.get() == 200)
                                        MockResponse().setBody("""{"id":"registered-device"}""")
                                    else
                                        MockResponse()
                                            .setResponseCode(registrationStatus.get())
                                            .setBody("""{"error":"Registration unavailable"}""")
                                }
                                request.path ==
                                    "/api/account/notifications/subscriptions/registered-device" &&
                                    request.method == "DELETE" -> {
                                    removed += request.path!!
                                    MockResponse().setResponseCode(204)
                                }
                                else -> MockResponse().setResponseCode(404)
                            }
                    }
                server.start()
                val vault = MemoryVault()
                val origin = server.url("/").toString()
                vault.write(origin, "cairn_session=fixture; Path=/; Max-Age=3600")
                val registrar = NativeDeviceRegistrar(context, vault, origin, tokens)
                registrar.enable()
                registrar.register()
                assertEquals(1, registrations.size)
                token = "second-token"
                registrar.register()
                assertEquals(2, registrations.size)
                assertEquals(registrations[0]["deviceId"], registrations[1]["deviceId"])
                assertEquals("second-token", registrations[1]["token"])

                registrar.disable()
                assertEquals(1, removed.size)
                assertEquals("deleted", token)
                assertFalse(NotificationPreferences(context).enabled.first())

                token = "reenrolled-token"
                registrar.enable()
                token = "third-token"
                registrationStatus.set(503)
                try {
                    registrar.register()
                    fail("A temporary failure must remain retryable")
                } catch (error: ApiException) {
                    assertEquals(503, error.status)
                }
                assertTrue(NotificationPreferences(context).enabled.first())
                assertFalse(NotificationPreferences(context).nativeReenrollmentRequired.first())

                // A delayed refusal must not undo a newer explicit recovery in the same session.
                token = "stale-token"
                val delayedRenewal =
                    async(Dispatchers.Default) {
                        try {
                            registrar.register()
                            fail("The old renewal must have received its refusal")
                        } catch (error: ApiException) {
                            assertEquals(403, error.status)
                        }
                    }
                try {
                    assertTrue(
                        withContext(Dispatchers.IO) {
                            delayedFailureStarted.await(10, TimeUnit.SECONDS)
                        }
                    )
                    token = "recovered-token"
                    registrationStatus.set(200)
                    registrar.enable()
                } finally {
                    releaseDelayedFailure.countDown()
                }
                delayedRenewal.await()
                assertTrue(
                    "An old refusal must not disable a successful recovery",
                    NotificationPreferences(context).enabled.first(),
                )
                assertFalse(NotificationPreferences(context).nativeReenrollmentRequired.first())
                val registrationsAfterRecovery = registrations.size
                registrar.register()
                assertEquals(registrationsAfterRecovery, registrations.size)

                token = "fourth-token"
                registrationStatus.set(403)
                try {
                    registrar.register()
                } catch (error: ApiException) {
                    assertEquals(403, error.status)
                }
                assertFalse(
                    "Refused re-enrollment must turn push off",
                    NotificationPreferences(context).enabled.first(),
                )

                assertTrue(NotificationPreferences(context).nativeReenrollmentRequired.first())
                registrar.disable()
                assertEquals(1, removed.size)
                assertEquals("deleted", token)
                assertFalse(NotificationPreferences(context).enabled.first())
                assertFalse(NotificationPreferences(context).nativeReenrollmentRequired.first())
            }
        }
}
