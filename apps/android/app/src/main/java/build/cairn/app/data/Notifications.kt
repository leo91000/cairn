package build.cairn.app.data

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.longPreferencesKey
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.core.stringSetPreferencesKey
import androidx.work.WorkManager
import java.util.UUID
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map

const val QUESTION_CHANNEL = "cairn-questions"
const val EXECUTION_CHANNEL = "cairn-execution"
const val NATIVE_PUSH_REENROLLMENT_MESSAGE =
    "Les notifications push ont été désactivées. Confirmez votre identité par e-mail ou passkey dans les réglages du compte, puis réactivez-les."
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
            it[nativeRegistrationRevisionKey] = UUID.randomUUID().toString()
        }
        schedule(context, value)
    }

    internal suspend fun requireNativeReenrollment(expectedRevision: String?) {
        var refused = false
        context.dataStore.edit {
            // A newer registration or explicit preference change wins over an old refusal.
            if (it[nativeRegistrationRevisionKey] != expectedRevision) return@edit

            it[enabledKey] = false
            it[nativeReenrollmentKey] = true
            it.remove(nativeRegistrationKey)
            it.remove(nativeRegistrationScopeKey)
            refused = true
        }

        // Do not cancel the renewal worker itself: it still has to finish handling its 403.
        if (refused) NotificationManagerCompat.from(context).cancelAll()
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
    if (enabled) enqueueNativeDevice(context)
    else {
        manager.cancelUniqueWork("cairn-native-device")
        NotificationManagerCompat.from(context).cancelAll()
    }
}
