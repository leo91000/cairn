package dev.leo.manager.data

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.core.net.toUri
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.longPreferencesKey
import androidx.datastore.preferences.core.stringSetPreferencesKey
import androidx.work.*
import dev.leo.manager.BuildConfig
import dev.leo.manager.MainActivity
import dev.leo.manager.R
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map

const val QUESTION_CHANNEL = "leo-questions"
const val EXECUTION_CHANNEL = "leo-execution"
private const val QUESTION_WORK = "leo-question-check"
private val enabledKey = booleanPreferencesKey("notifications")
private val seenKey = stringSetPreferencesKey("notified_questions")
private val alertsKey = longPreferencesKey("notified_node_alerts_until")

class NotificationPreferences(private val context: Context) {
    val enabled = context.dataStore.data.map { it[enabledKey] ?: false }

    suspend fun setEnabled(value: Boolean) {
        context.dataStore.edit { it[enabledKey] = value }
        schedule(context, value)
    }

    suspend fun seen() = context.dataStore.data.first()[seenKey].orEmpty()

    suspend fun setSeen(ids: Set<String>) {
        context.dataStore.edit { it[seenKey] = ids }
    }

    /** Creation time of the newest node alert already shown; null before the first check. */
    suspend fun alertsUntil() = context.dataStore.data.first()[alertsKey]

    suspend fun setAlertsUntil(value: Long) {
        context.dataStore.edit { it[alertsKey] = value }
    }
}

fun notificationsAllowed(context: Context): Boolean =
    (Build.VERSION.SDK_INT < 33 ||
        ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED) &&
        NotificationManagerCompat.from(context).areNotificationsEnabled()

fun schedule(context: Context, enabled: Boolean) {
    val manager = WorkManager.getInstance(context)
    if (enabled)
        manager.enqueueUniquePeriodicWork(
            QUESTION_WORK,
            ExistingPeriodicWorkPolicy.KEEP,
            PeriodicWorkRequestBuilder<QuestionWorker>(15, TimeUnit.MINUTES)
                .setConstraints(
                    Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build()
                )
                .build(),
        )
    else {
        manager.cancelUniqueWork(QUESTION_WORK)
        NotificationManagerCompat.from(context).cancelAll()
    }
}

/**
 * Fetches pending question identifiers only; answers never enter notifications or persistent work
 * data.
 */
class QuestionWorker
@JvmOverloads
constructor(
    context: Context,
    params: WorkerParameters,
    private val vault: SessionVault = KeystoreSessionVault(context),
) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result {
        val context = applicationContext
        val prefs = NotificationPreferences(context)
        if (!prefs.enabled.first() || !notificationsAllowed(context)) return Result.success()
        val origin = Preferences(context).origin.first()
        if (origin.isBlank()) return Result.success()
        val originalCookie = vault.read(origin) ?: return Result.success()
        try {
            val api = LeoApi(serverOrigin(origin, BuildConfig.DEBUG), vault)
            val session = api.get<Session>("/session")
            if (!session.authenticated) return Result.success()
            val chats = api.get<List<Chat>>("/chats").filter { it.pendingQuestions > 0 }
            val pending = mutableMapOf<String, Set<String>>()
            for (chat in chats) {
                val detail = api.get<Chat>("/chats/${segment(chat.id)}")
                pending[chat.id] =
                    detail.questions.filter { it.status == "pending" }.map { it.id }.toSet()
            }
            // Logout, server changes and disabling notifications win over an in-flight check.
            if (
                !prefs.enabled.first() ||
                    Preferences(context).origin.first() != origin ||
                    vault.read(origin) != originalCookie
            )
                return Result.success()
            val manager = context.getSystemService(NotificationManager::class.java)
            manager.createNotificationChannel(
                NotificationChannel(
                    QUESTION_CHANNEL,
                    "Questions des agents",
                    NotificationManager.IMPORTANCE_DEFAULT,
                )
            )
            val previous = prefs.seen()
            val all = pending.values.flatten().toSet()
            manager.activeNotifications
                .filter { it.notification.channelId == QUESTION_CHANNEL && it.tag !in pending.keys }
                .forEach { manager.cancel(it.tag, it.id) }
            for ((chat, questions) in pending) {
                if ((questions - previous).isEmpty()) continue
                val intent =
                    Intent(context, MainActivity::class.java)
                        .setAction("dev.leo.manager.OPEN_CHAT")
                        .setData(
                            "leo-manager://chat/${segment(chat)}?origin=${segment(origin)}".toUri()
                        )
                        .putExtra("chat", chat)
                        .putExtra("origin", origin)
                        .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
                val action =
                    PendingIntent.getActivity(
                        context,
                        0,
                        intent,
                        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
                    )
                val notification =
                    NotificationCompat.Builder(context, QUESTION_CHANNEL)
                        .setSmallIcon(R.drawable.ic_leo)
                        .setContentTitle("Leo attend votre réponse")
                        .setContentText(
                            if (questions.size == 1)
                                "Une question vous attend dans une conversation."
                            else "${questions.size} questions vous attendent dans une conversation."
                        )
                        .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
                        .setContentIntent(action)
                        .setAutoCancel(true)
                        .build()
                if (notificationsAllowed(context)) manager.notify(chat, 1, notification)
            }
            prefs.setSeen(all)
            // A master without node alerts must not delay question notifications.
            val alerts =
                try {
                    api.get<List<NodeAlert>>("/nodes/alerts")
                } catch (e: CancellationException) {
                    throw e
                } catch (_: Exception) {
                    null
                }
            alerts?.let { notifyAlerts(context, manager, prefs, it, origin) }
            return Result.success()
        } catch (e: CancellationException) {
            throw e
        } catch (e: ApiException) {
            return if (e.status in setOf(401, 403)) Result.success() else Result.retry()
        } catch (_: Exception) {
            return Result.retry()
        }
    }
}

/**
 * Node alerts (conversation waiting for its node, failover, failed recovery point) carry only a
 * short fixed text. The first check only records the newest alert so old events are not replayed.
 */
private suspend fun notifyAlerts(
    context: Context,
    manager: NotificationManager,
    prefs: NotificationPreferences,
    alerts: List<NodeAlert>,
    origin: String,
) {
    val newest = alerts.maxOfOrNull { it.createdAt } ?: return
    val until = prefs.alertsUntil()
    prefs.setAlertsUntil(maxOf(newest, until ?: 0))
    if (until == null) return
    manager.createNotificationChannel(
        NotificationChannel(EXECUTION_CHANNEL, "Exécution des conversations", NotificationManager.IMPORTANCE_DEFAULT)
    )
    for (alert in alerts.filter { it.createdAt > until }.sortedBy { it.createdAt }.takeLast(5)) {
        val intent =
            Intent(context, MainActivity::class.java)
                .setAction("dev.leo.manager.OPEN_CHAT")
                .setData("leo-manager://chat/${segment(alert.chatId)}?origin=${segment(origin)}".toUri())
                .putExtra("chat", alert.chatId)
                .putExtra("origin", origin)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        val notification =
            NotificationCompat.Builder(context, EXECUTION_CHANNEL)
                .setSmallIcon(R.drawable.ic_leo)
                .setContentTitle(alert.localized().first.take(120))
                .setContentText(alert.localized().second.take(300))
                .setStyle(NotificationCompat.BigTextStyle().bigText(alert.localized().second.take(300)))
                .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
                .setContentIntent(PendingIntent.getActivity(context, alert.id.hashCode(), intent, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE))
                .setAutoCancel(true)
                .build()
        if (notificationsAllowed(context)) manager.notify("node-${alert.id}", 2, notification)
    }
}
