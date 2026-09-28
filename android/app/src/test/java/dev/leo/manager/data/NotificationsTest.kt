package dev.leo.manager.data

import android.Manifest
import android.app.Application
import android.app.NotificationManager
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.work.*
import androidx.work.testing.*
import kotlinx.coroutines.runBlocking
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class NotificationsTest {
    @Test
    fun `periodic checks deduplicate questions use scoped chat intents and stop when disabled`() =
        runBlocking {
            val context = ApplicationProvider.getApplicationContext<Application>()
            shadowOf(context).grantPermissions(Manifest.permission.POST_NOTIFICATIONS)
            WorkManagerTestInitHelper.initializeTestWorkManager(
                context,
                Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
            )
            MockWebServer().use { server ->
                var pending = true
                server.dispatcher =
                    object : Dispatcher() {
                        override fun dispatch(request: RecordedRequest): MockResponse {
                            if (request.getHeader("Cookie") != "leo_session=notification-fixture")
                                return MockResponse().setResponseCode(401)
                            return MockResponse()
                                .setBody(
                                    when (request.path) {
                                        "/api/session" -> """{"authenticated":true}"""
                                        "/api/chats" ->
                                            if (pending) """[{"id":"chat","pendingQuestions":1}]"""
                                            else "[]"
                                        "/api/chats/chat" ->
                                            """{"id":"chat","questions":[{"id":"q","chatId":"chat","fields":[{"id":"private","title":"Do not show in notifications","secret":true}]}]}"""
                                        else -> "{}"
                                    }
                                )
                        }
                    }
                server.start()
                val origin = server.url("/").toString()
                Preferences(context).setOrigin(origin)
                val prefs = NotificationPreferences(context)
                prefs.setEnabled(true)
                val vault =
                    MemoryVault().apply {
                        write(origin, "leo_session=notification-fixture; Path=/; Max-Age=3600")
                    }
                val worker =
                    TestListenableWorkerBuilder<QuestionWorker>(context)
                        .setWorkerFactory(
                            object : WorkerFactory() {
                                override fun createWorker(
                                    appContext: Context,
                                    workerClassName: String,
                                    workerParameters: WorkerParameters,
                                ): ListenableWorker =
                                    QuestionWorker(appContext, workerParameters, vault)
                            }
                        )
                        .build()
                assertEquals(ListenableWorker.Result.success(), worker.doWork())
                val manager = context.getSystemService(NotificationManager::class.java)
                val notification = manager.activeNotifications.single().notification
                val intent = shadowOf(notification.contentIntent).savedIntent
                assertEquals("chat", intent.getStringExtra("chat"))
                assertEquals(origin, intent.getStringExtra("origin"))
                assertFalse(notification.extras.toString().contains("Do not show"))
                assertEquals(setOf("q"), prefs.seen())
                manager.cancelAll()
                assertEquals(ListenableWorker.Result.success(), worker.doWork())
                assertTrue(manager.activeNotifications.isEmpty())
                pending = false
                assertEquals(ListenableWorker.Result.success(), worker.doWork())
                assertTrue(prefs.seen().isEmpty())
                prefs.setEnabled(false)
                val requests = server.requestCount
                assertEquals(ListenableWorker.Result.success(), worker.doWork())
                assertEquals(requests, server.requestCount)
            }
            WorkManagerTestInitHelper.closeWorkDatabase()
        }

    @Test
    fun `node alerts notify once in French without replaying older events`() = runBlocking {
        val context = ApplicationProvider.getApplicationContext<Application>()
        shadowOf(context).grantPermissions(Manifest.permission.POST_NOTIFICATIONS)
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        MockWebServer().use { server ->
            var alerts = """[{"id":"old","chatId":"chat","kind":"waiting","title":"Old","body":"Old","createdAt":1}]"""
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest) =
                        MockResponse()
                            .setBody(
                                when (request.path) {
                                    "/api/session" -> """{"authenticated":true}"""
                                    "/api/nodes/alerts" -> alerts
                                    else -> "[]"
                                }
                            )
                }
            server.start()
            val origin = server.url("/").toString()
            Preferences(context).setOrigin(origin)
            NotificationPreferences(context).setEnabled(true)
            val vault = MemoryVault().apply { write(origin, "leo_session=fixture; Path=/; Max-Age=3600") }
            val worker =
                TestListenableWorkerBuilder<QuestionWorker>(context)
                    .setWorkerFactory(
                        object : WorkerFactory() {
                            override fun createWorker(
                                appContext: Context,
                                workerClassName: String,
                                workerParameters: WorkerParameters,
                            ): ListenableWorker = QuestionWorker(appContext, workerParameters, vault)
                        }
                    )
                    .build()
            val manager = context.getSystemService(NotificationManager::class.java)
            assertEquals(ListenableWorker.Result.success(), worker.doWork())
            assertTrue(manager.activeNotifications.isEmpty())
            alerts = """[{"id":"new","chatId":"chat","kind":"resumed","title":"Resumed","body":"English","createdAt":2},{"id":"old","chatId":"chat","kind":"waiting","createdAt":1}]"""
            assertEquals(ListenableWorker.Result.success(), worker.doWork())
            val notification = manager.activeNotifications.single().notification
            assertEquals("Conversation reprise sur une autre node", notification.extras.getString("android.title"))
            assertEquals("chat", shadowOf(notification.contentIntent).savedIntent.getStringExtra("chat"))
            manager.cancelAll()
            assertEquals(ListenableWorker.Result.success(), worker.doWork())
            assertTrue(manager.activeNotifications.isEmpty())
        }
        WorkManagerTestInitHelper.closeWorkDatabase()
    }
}
