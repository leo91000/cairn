package dev.leo.manager.data

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import java.util.UUID
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
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
                                    registrations +=
                                        wireJson
                                            .parseToJsonElement(request.body.readUtf8())
                                            .jsonObject
                                            .mapValues { it.value.jsonPrimitive.content }
                                    MockResponse().setBody("""{"id":"registered-device"}""")
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
                vault.write(origin, "leo_session=fixture; Path=/; Max-Age=3600")
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
            }
        }
}
