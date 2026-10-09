package build.cairn.app.data

import android.app.Notification
import android.app.NotificationManager
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import java.util.UUID
import kotlinx.coroutines.runBlocking
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.*

abstract class NativePushCases {
    open fun sessionVault(context: Context): SessionVault =
        object : SessionVault {
            private val values = mutableMapOf<String, String>()

            override fun read(origin: String) = values[origin]

            override fun write(origin: String, cookie: String?) {
                if (cookie == null) values.remove(origin) else values[origin] = cookie
            }
        }

    fun bothInstallationsNotifyButRemovedMembersAndLoggedOutDevicesDoNot() = runBlocking {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.cancelAll()
        val account = UUID.randomUUID().toString()
        val first = UUID.randomUUID().toString()
        val second = UUID.randomUUID().toString()
        val accessible = java.util.concurrent.atomic.AtomicReference(listOf(first, second))
        val owner = java.util.concurrent.atomic.AtomicBoolean(false)
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        assertEquals("/api/account/session", request.path)
                        assertTrue(
                            request.getHeader("Cookie").orEmpty().contains("cairn_session=fixture")
                        )
                        val installations =
                            accessible.get().joinToString(",") {
                                """{"id":"$it","name":"Installation","role":"${if (owner.get() && it == first) "owner" else "member"}","online":true}"""
                            }
                        return MockResponse()
                            .setBody(
                                """{"authenticated":true,"csrf":"csrf","account":{"id":"$account","email":"alice@example.test"},"installations":[$installations]}"""
                            )
                    }
                }
            server.start()
            val origin = server.url("/").toString()
            val vault = sessionVault(context)
            vault.write(origin, "cairn_session=fixture; Path=/; Max-Age=3600")
            val preferences = NotificationPreferences(context)
            preferences.setEnabled(true)
            val receiver = NativeNotificationReceiver(context, vault, origin)
            fun message(installation: String, suffix: Char) =
                mapOf(
                    "accountId" to account,
                    "installationId" to installation,
                    "chatId" to UUID.randomUUID().toString(),
                    "questionId" to suffix.toString().repeat(64),
                    "title" to "PRIVATE TEXT MUST NOT APPEAR",
                )
            assertTrue(receiver.receive(message(first, 'a')))
            assertTrue(receiver.receive(message(second, 'b')))
            awaitNotificationCount(manager, 2)
            assertEquals(2, deliveredNotifications(manager).size)
            deliveredNotifications(manager).forEach {
                assertEquals(
                    "Cairn attend votre réponse",
                    it.notification.extras.getString("android.title"),
                )
                assertFalse(it.notification.extras.toString().contains("PRIVATE TEXT"))
            }
            val alert =
                message(first, 'e').minus("questionId") +
                    ("alertId" to UUID.randomUUID().toString())
            assertFalse(receiver.receive(alert))
            owner.set(true)
            assertTrue(receiver.receive(alert))
            awaitNotificationCount(manager, 3)
            accessible.set(listOf(first))
            assertFalse(receiver.receive(message(second, 'c')))
            assertEquals(3, deliveredNotifications(manager).size)
            vault.write(origin, null)
            assertFalse(receiver.receive(message(first, 'd')))
            assertEquals(3, deliveredNotifications(manager).size)
            preferences.setEnabled(false)
        }
        manager.cancelAll()
    }

    // Android may add a group summary after the third message; it is not another delivery.
    private fun deliveredNotifications(manager: NotificationManager) =
        manager.activeNotifications.filter {
            it.notification.flags and Notification.FLAG_GROUP_SUMMARY == 0
        }

    private suspend fun awaitNotificationCount(manager: NotificationManager, expected: Int) {
        try {
            kotlinx.coroutines.withTimeout(5000) {
                while (deliveredNotifications(manager).size != expected) kotlinx.coroutines.delay(
                    25
                )
            }
        } catch (timeout: kotlinx.coroutines.TimeoutCancellationException) {
            val observed =
                manager.activeNotifications.map {
                    "id=${it.id}, flags=${it.notification.flags}, channel=${it.notification.channelId}"
                }
            throw AssertionError("Expected $expected notifications; observed $observed", timeout)
        }
    }
}
