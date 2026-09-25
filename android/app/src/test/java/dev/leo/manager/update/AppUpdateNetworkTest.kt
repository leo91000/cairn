package dev.leo.manager.update

import android.content.pm.ApplicationInfo
import android.content.pm.PackageInfo
import android.content.pm.Signature
import dev.leo.manager.BuildConfig
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import okhttp3.OkHttpClient
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [26])
@Suppress("DEPRECATION")
class AppUpdateNetworkTest {
    private val update =
        AppUpdate(
            1,
            "0.30.0",
            BuildConfig.VERSION_CODE + 1,
            26,
            "$RELEASE_BASE/download/v0.30.0/leo-android.apk",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            3,
        )

    @Test
    fun checksVersionWithoutSendingSessionCredentials() = runBlocking {
        MockWebServer().use { server ->
            val client =
                OkHttpClient.Builder()
                    .addInterceptor { chain ->
                        assertNull(chain.request().header("Cookie"))
                        assertNull(chain.request().header("Authorization"))
                        chain.proceed(
                            chain.request().newBuilder().url(server.url("/update")).build()
                        )
                    }
                    .build()
            val updater = AppUpdateClient(RuntimeEnvironment.getApplication(), client)
            server.enqueue(MockResponse().setBody(Json.encodeToString(update)))
            assertEquals(update, updater.check())
            server.enqueue(
                MockResponse()
                    .setBody(
                        Json.encodeToString(update.copy(versionCode = BuildConfig.VERSION_CODE))
                    )
            )
            assertNull(updater.check())
            server.enqueue(MockResponse().setResponseCode(404))
            assertNull(updater.check())
            server.enqueue(MockResponse().setBody("x".repeat(17 * 1024)))
            assertTrue(runCatching { updater.check() }.isFailure)
            server.enqueue(MockResponse().setResponseCode(503))
            assertTrue(runCatching { updater.check() }.isFailure)
        }
    }

    private fun info(code: Int, signer: String = "aabb") =
        PackageInfo().apply {
            packageName = "dev.leo.manager"
            versionName = "0.30.0"
            versionCode = code
            signatures = arrayOf(Signature(signer))
            applicationInfo = ApplicationInfo().apply { minSdkVersion = 26 }
        }

    @Test
    fun refusesWrongSignerPackageVersionAndUnsupportedSdk() {
        val installed = info(28)
        validateApkIdentity(info(update.versionCode), installed, update, 26)
        val invalid =
            listOf(
                info(update.versionCode, "ccdd"),
                info(update.versionCode).apply { packageName = "another.app" },
                info(update.versionCode + 1),
                info(update.versionCode).apply { versionName = "0.30.1" },
                info(update.versionCode).apply { applicationInfo!!.minSdkVersion = 27 },
                info(update.versionCode).apply { signatures = emptyArray() },
            )
        invalid.forEach { archive ->
            assertThrows(IllegalArgumentException::class.java) {
                validateApkIdentity(archive, installed, update, 26)
            }
        }
        assertThrows(IllegalArgumentException::class.java) {
            validateApkIdentity(info(update.versionCode), info(update.versionCode), update, 26)
        }
    }
}
