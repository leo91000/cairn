package build.cairn.app.data

import android.content.Context
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import build.cairn.app.BuildConfig
import java.security.MessageDigest
import java.util.UUID
import kotlinx.coroutines.flow.first
import kotlinx.serialization.Serializable

interface PushTokens {
    val available: Boolean

    suspend fun token(): String

    suspend fun delete()
}

private val nativeDeviceKey = stringPreferencesKey("native_push_device")
internal val nativeRegistrationKey = stringPreferencesKey("native_push_registration")
internal val nativeRegistrationScopeKey = stringPreferencesKey("native_push_registration_scope")
internal val nativeRegistrationRevisionKey =
    stringPreferencesKey("native_push_registration_revision")

@Serializable private data class AndroidPushConfiguration(val enabled: Boolean = false)

@Serializable private data class DeviceRegistration(val registered: Boolean = false)

@Serializable private data class RegisteredDevice(val id: String)

/** One account/device registration is independent of the selected installation. */
class NativeDeviceRegistrar(
    private val context: Context,
    private val vault: SessionVault = KeystoreSessionVault(context),
    private val beaconOrigin: String = BuildConfig.BEACON_SERVICE_ORIGIN,
    private val tokens: PushTokens = FirebasePushTokens(),
) {
    suspend fun enable() {
        require(tokens.available && beaconOrigin.isNotBlank()) {
            "Les notifications push ne sont pas disponibles dans cette version."
        }
        require(notificationsAllowed(context)) { "Autorisez les notifications dans Android." }
        try {
            registerDevice()
            NotificationPreferences(context).setEnabled(true)
        } catch (error: Exception) {
            try {
                if (tokens.available) tokens.delete()
            } catch (_: Exception) {
                /* Keep the original registration error. */
            }
            throw error
        }
    }

    suspend fun register() {
        if (!NotificationPreferences(context).enabled.first() || !notificationsAllowed(context))
            return
        if (!tokens.available || beaconOrigin.isBlank()) return
        registerDevice()
    }

    private suspend fun registerDevice() {
        val origin = serverOrigin(beaconOrigin, BuildConfig.DEBUG)
        val originalCookie =
            vault.read(origin.toString()) ?: error("Connectez-vous à votre compte Cairn.")
        val api = CairnApi(origin, vault)
        val session = api.get<Session>("/account/session")
        require(session.authenticated && session.account != null) {
            "Connectez-vous à votre compte Cairn."
        }
        api.csrf = session.csrf.orEmpty()
        require(api.get<AndroidPushConfiguration>("/account/notifications/android").enabled) {
            "Beacon ne propose pas les notifications Android actuellement."
        }
        val token = tokens.token()
        val fingerprint =
            MessageDigest.getInstance("SHA-256").digest(token.toByteArray()).joinToString("") {
                "%02x".format(it)
            }
        val scope = "$origin:${session.account.id}:$fingerprint"
        val stored = context.dataStore.data.first()
        val existingId = stored[nativeRegistrationKey]
        if (
            stored[nativeRegistrationScopeKey] == scope &&
                existingId != null &&
                api.get<DeviceRegistration>(
                        "/account/notifications/subscriptions/${segment(existingId)}"
                    )
                    .registered
        )
            return
        var deviceId = ""
        context.dataStore.edit {
            deviceId =
                it[nativeDeviceKey]
                    ?: UUID.randomUUID().toString().also { id -> it[nativeDeviceKey] = id }
        }
        require(vault.read(origin.toString()) == originalCookie) {
            "La session du compte Cairn a changé."
        }
        val registration =
            try {
                api.send<RegisteredDevice>(
                    "POST",
                    "/account/notifications/android",
                    body("deviceId" to deviceId, "token" to token),
                )
            } catch (error: ApiException) {
                if (error.status == 403 && vault.read(origin.toString()) == originalCookie) {
                    NotificationPreferences(context)
                        .requireNativeReenrollment(stored[nativeRegistrationRevisionKey])
                }

                throw error
            }
        require(vault.read(origin.toString()) == originalCookie) {
            "La session du compte Cairn a changé."
        }
        context.dataStore.edit {
            it[nativeRegistrationKey] = registration.id
            it[nativeRegistrationScopeKey] = scope
            it[nativeRegistrationRevisionKey] = UUID.randomUUID().toString()
        }
    }

    suspend fun disable() {
        NotificationPreferences(context).setEnabled(false)
        try {
            val stored = context.dataStore.data.first()
            val id = stored[nativeRegistrationKey]
            if (id != null && beaconOrigin.isNotBlank()) {
                val api = CairnApi(serverOrigin(beaconOrigin, BuildConfig.DEBUG), vault)
                val session = api.get<Session>("/account/session")
                if (session.authenticated) {
                    api.csrf = session.csrf.orEmpty()
                    api.request("DELETE", "/account/notifications/subscriptions/${segment(id)}")
                }
            }
        } finally {
            context.dataStore.edit {
                it.remove(nativeRegistrationKey)
                it.remove(nativeRegistrationScopeKey)
            }
            if (tokens.available) tokens.delete()
        }
    }
}
