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
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.core.stringSetPreferencesKey
import androidx.work.*
import dev.leo.manager.BuildConfig
import dev.leo.manager.MainActivity
import dev.leo.manager.R
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map

const val QUESTION_CHANNEL = "leo-questions"
const val EXECUTION_CHANNEL = "leo-execution"
const val NATIVE_PUSH_REENROLLMENT_MESSAGE =
    "Les notifications push ont été désactivées. Confirmez votre identité par e-mail ou passkey dans les réglages du compte, puis réactivez-les."
private const val QUESTION_WORK = "leo-question-check"
private val enabledKey = booleanPreferencesKey("notifications")
private val nativeReenrollmentKey = booleanPreferencesKey("native_push_reenrollment_required")
private val seenKey = stringSetPreferencesKey("notified_questions")
private val scopeKey = stringPreferencesKey("notification_installation_scope")
private val alertsKey = longPreferencesKey("notified_node_alerts_until")

class NotificationPreferences(private val context: Context) {
    val enabled = context.dataStore.data.map { it[enabledKey] ?: false }
    val nativeReenrollmentRequired =
        context.dataStore.data.map { it[nativeReenrollmentKey] ?: false }

    suspend fun setEnabled(value: Boolean) {
        context.dataStore.edit {
            it[enabledKey] = value
            it.remove(nativeReenrollmentKey)
        }
        schedule(context, value)
    }

    internal suspend fun requireNativeReenrollment() {
        context.dataStore.edit {
            it[enabledKey] = false
            it[nativeReenrollmentKey] = true
        }
        // Do not cancel the renewal worker itself: it still has to finish handling its 403.
        NotificationManagerCompat.from(context).cancelAll()
    }

    suspend fun selectScope(scope: String): Boolean {
        var changed = false
        context.dataStore.edit {
            if (it[scopeKey] != scope) {
                it[scopeKey] = scope
                it.remove(seenKey)
                it.remove(alertsKey)
                changed = true
            }
        }
        return changed
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
    // Upgrade cancels the legacy per-installation polling; native push belongs to the account.
    manager.cancelUniqueWork(QUESTION_WORK)
    if (enabled) enqueueNativeDevice(context)
    else {
        manager.cancelUniqueWork("leo-native-device")
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
    private val officialOrigin: String = BuildConfig.OFFICIAL_SERVICE_ORIGIN,
) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result {
        val context = applicationContext
        val prefs = NotificationPreferences(context)
        if (!prefs.enabled.first() || !notificationsAllowed(context)) return Result.success()
        if (officialOrigin.isBlank()) return Result.success()
        val origin = serverOrigin(officialOrigin, BuildConfig.DEBUG).toString()
        val originalCookie = vault.read(origin) ?: return Result.success()
        try {
            val account = LeoApi(serverOrigin(origin, BuildConfig.DEBUG), vault)
            val session = account.get<Session>("/account/session")
            if (!session.authenticated || session.account == null) return Result.success()
            val preferences = Preferences(context)
            val saved = preferences.lastInstallation(origin, session.account.id)
            val installation =
                if (saved != null) session.installations.find { it.id == saved }
                else session.installations.firstOrNull()
            if (installation == null || !installation.online) return Result.success()
            val api = LeoApi(account.origin, vault, installationId = installation.id)
            api.csrf = session.csrf.orEmpty()
            val chats = api.get<List<Chat>>("/chats").filter { it.pendingQuestions > 0 }
            val pending = mutableMapOf<String, Set<String>>()
            for (chat in chats) {
                val detail = api.get<Chat>("/chats/${segment(chat.id)}")
                pending[chat.id] =
                    detail.questions.filter { it.status == "pending" }.map { it.id }.toSet()
            }
            // Logout, installation changes and disabling notifications win over an in-flight check.
            if (
                !prefs.enabled.first() ||
                    preferences.lastInstallation(origin, session.account.id) != saved ||
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
                            "leo-manager://chat/${segment(chat)}?origin=${segment(api.cacheScope)}"
                                .toUri()
                        )
                        .putExtra("chat", chat)
                        .putExtra("origin", api.cacheScope)
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
                if (installation.role != InstallationRole.Owner) null
                else
                    try {
                        api.get<List<NodeAlert>>("/nodes/alerts")
                    } catch (e: CancellationException) {
                        throw e
                    } catch (_: Exception) {
                        null
                    }
            alerts?.let { notifyAlerts(context, manager, prefs, it, api.cacheScope) }
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
    cacheScope: String,
) {
    val newest = alerts.maxOfOrNull { it.createdAt } ?: return
    val until = prefs.alertsUntil()
    prefs.setAlertsUntil(maxOf(newest, until ?: 0))
    if (until == null) return
    manager.createNotificationChannel(
        NotificationChannel(
            EXECUTION_CHANNEL,
            "Exécution des conversations",
            NotificationManager.IMPORTANCE_DEFAULT,
        )
    )
    for (alert in alerts.filter { it.createdAt > until }.sortedBy { it.createdAt }.takeLast(5)) {
        val intent =
            Intent(context, MainActivity::class.java)
                .setAction("dev.leo.manager.OPEN_CHAT")
                .setData(
                    "leo-manager://chat/${segment(alert.chatId)}?origin=${segment(cacheScope)}"
                        .toUri()
                )
                .putExtra("chat", alert.chatId)
                .putExtra("origin", cacheScope)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        val notification =
            NotificationCompat.Builder(context, EXECUTION_CHANNEL)
                .setSmallIcon(R.drawable.ic_leo)
                .setContentTitle(alert.localized().first.take(120))
                .setContentText(alert.localized().second.take(300))
                .setStyle(
                    NotificationCompat.BigTextStyle().bigText(alert.localized().second.take(300))
                )
                .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
                .setContentIntent(
                    PendingIntent.getActivity(
                        context,
                        alert.id.hashCode(),
                        intent,
                        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
                    )
                )
                .setAutoCancel(true)
                .build()
        if (notificationsAllowed(context)) manager.notify("node-${alert.id}", 2, notification)
    }
}
