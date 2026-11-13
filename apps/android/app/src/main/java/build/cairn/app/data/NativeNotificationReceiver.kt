package build.cairn.app.data

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import androidx.core.app.NotificationCompat
import androidx.core.net.toUri
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringSetPreferencesKey
import build.cairn.app.BuildConfig
import build.cairn.app.MainActivity
import build.cairn.app.R
import java.util.UUID
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*

private val nativeSeenKey = stringSetPreferencesKey("native_push_seen")

/** Data-only push is authorized again before displaying it; no provider text is trusted. */
class NativeNotificationReceiver(
    private val context: Context,
    private val vault: SessionVault = KeystoreSessionVault(context),
    private val beaconOrigin: String = BuildConfig.BEACON_SERVICE_ORIGIN,
) {
    suspend fun receive(data: Map<String, String>): Boolean {
        val account = data["accountId"] ?: return false
        val installationId = data["installationId"] ?: return false
        val rule =
            data["ruleId"]?.takeIf {
                runCatching { UUID.fromString(it).toString() == it }.getOrDefault(false)
            }
        if (data.containsKey("ruleId") && rule == null) return false
        val chat = data["chatId"].orEmpty()
        if (
            (listOf(account, installationId) + if (rule == null) listOf(chat) else emptyList())
                .any {
                    runCatching { UUID.fromString(it).toString() == it }.getOrDefault(false).not()
                }
        )
            return false
        val question = data["questionId"]?.takeIf { Regex("[a-f0-9]{64}").matches(it) }
        val alert =
            data["alertId"]?.takeIf {
                runCatching { UUID.fromString(it).toString() == it }.getOrDefault(false)
            }
        if ((question == null) == (alert == null) || (rule != null && alert == null)) return false
        if (beaconOrigin.isBlank()) return false
        val preferences = NotificationPreferences(context)
        if (!preferences.enabled.first() || !notificationsAllowed(context)) return false
        val origin = serverOrigin(beaconOrigin, BuildConfig.DEBUG)
        val originalCookie = vault.read(origin.toString()) ?: return false
        val api = CairnApi(origin, vault)
        val session = api.get<Session>("/account/session")
        if (!session.authenticated || session.account?.id != account) return false
        val installation = session.installations.find { it.id == installationId } ?: return false
        if (alert != null && rule == null && installation.role != InstallationRole.Owner)
            return false
        if (rule != null) {
            val ruleApi = CairnApi(origin, vault, installationId = installationId)
            val current = ruleApi.get<JsonObject>("/hook-rules/${segment(rule)}")
            val mine = current["creatorAccountId"]?.jsonPrimitive?.contentOrNull == account
            val exhausted =
                (current["blocks"] as? JsonArray).orEmpty().any {
                    (it as? JsonObject)?.get("kind")?.jsonPrimitive?.contentOrNull ==
                        "budgetExhausted"
                }
            if (!mine || !exhausted) return false
        }
        val receipt = "$account:$installationId:${question ?: alert}"
        if (receipt in context.dataStore.data.first()[nativeSeenKey].orEmpty()) return false
        if (
            !preferences.enabled.first() ||
                !notificationsAllowed(context) ||
                vault.read(origin.toString()) != originalCookie
        )
            return false
        val channel = if (question != null) QUESTION_CHANNEL else EXECUTION_CHANNEL
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(
                channel,
                if (question != null) "Questions des agents" else "Exécution des conversations",
                NotificationManager.IMPORTANCE_DEFAULT,
            )
        )
        val cacheScope = "$origin$installationId"
        val intent =
            Intent(context, MainActivity::class.java)
                .setAction(
                    if (rule == null) "build.cairn.app.OPEN_CHAT"
                    else "build.cairn.app.OPEN_INSTALLATION"
                )
                .setData(
                    (if (rule == null)
                            "cairn-manager://chat/${segment(chat)}?origin=${segment(cacheScope)}&account=${segment(account)}"
                        else
                            "cairn-manager://installation/${segment(installationId)}?origin=${segment(cacheScope)}&account=${segment(account)}&rule=${segment(rule)}")
                        .toUri()
                )
                .putExtra("chat", chat)
                .putExtra("origin", cacheScope)
                .putExtra("account", account)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        val notification =
            NotificationCompat.Builder(context, channel)
                .setSmallIcon(R.drawable.ic_cairn)
                .setContentTitle(
                    if (rule != null) "Une règle a atteint sa limite horaire"
                    else if (question != null) "Cairn attend votre réponse"
                    else "Une conversation requiert votre attention"
                )
                .setContentText(
                    if (rule != null) "Ouvrez Cairn pour consulter ses règles."
                    else if (question != null) "Une question vous attend dans une conversation."
                    else "Consultez son état dans Cairn."
                )
                .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
                .setContentIntent(
                    PendingIntent.getActivity(
                        context,
                        0,
                        intent,
                        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
                    )
                )
                .setAutoCancel(true)
                .build()
        manager.notify(
            "$account:$installationId:${if (rule != null) "rule:$rule" else chat}",
            if (question != null) 1 else 2,
            notification,
        )
        context.dataStore.edit {
            it[nativeSeenKey] =
                (it[nativeSeenKey].orEmpty().toList() + receipt).takeLast(512).toSet()
        }
        return true
    }
}
