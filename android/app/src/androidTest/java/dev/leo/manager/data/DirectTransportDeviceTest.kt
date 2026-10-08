package dev.leo.manager.data

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.*
import okhttp3.HttpUrl.Companion.toHttpUrl
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

/** Real native libwebrtc against the existing installation/control-plane bench fixture. */
@RunWith(AndroidJUnit4::class)
class DirectTransportDeviceTest {
    @Test
    fun authorizedInstallationCarriesAnObservedDirectResponse() = runBlocking {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
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
        val control = java.util.concurrent.ConcurrentLinkedQueue<String>()
        val client =
            okhttp3.OkHttpClient.Builder()
                .addNetworkInterceptor { chain ->
                    val response = chain.proceed(chain.request())
                    val path = chain.request().url.encodedPath
                    if (path.contains("/direct/"))
                        control.add(path.substringAfterLast('/') + ":" + response.code)
                    response
                }
                .build()
        val api =
            LeoApi(
                origin,
                vault,
                client,
                installationId = configuration["installation"]!!.jsonPrimitive.content,
            )
        api.csrf = configuration["csrf"]!!.jsonPrimitive.content
        try {
            api.request("GET", "/chats")
            assertEquals("relay", api.transport.route.value)
            api.startDirect(context)
            withTimeout(20000) {
                while (api.transport.route.value != "direct") {
                    delay(200)
                    api.request("GET", "/chats")
                }
            }
            assertEquals("direct", api.transport.route.value)
        } finally {
            api.closeStreams()
        }
    }
}
