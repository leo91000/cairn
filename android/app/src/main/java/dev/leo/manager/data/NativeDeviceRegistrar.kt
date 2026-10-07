package dev.leo.manager.data

import android.content.Context
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import dev.leo.manager.BuildConfig
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
private val nativeRegistrationKey = stringPreferencesKey("native_push_registration")
private val nativeRegistrationScopeKey = stringPreferencesKey("native_push_registration_scope")

@Serializable private data class AndroidPushConfiguration(val enabled: Boolean = false)

@Serializable private data class DeviceRegistration(val registered: Boolean = false)

@Serializable private data class RegisteredDevice(val id: String)

/** One account/device registration is independent of the selected installation. */
class NativeDeviceRegistrar(
    private val context: Context,
    private val vault: SessionVault = KeystoreSessionVault(context),
    private val officialOrigin: String = BuildConfig.OFFICIAL_SERVICE_ORIGIN,
    private val tokens: PushTokens = FirebasePushTokens(),
) {
    suspend fun enable() {
        require(tokens.available && officialOrigin.isNotBlank()) {
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
        if (!tokens.available || officialOrigin.isBlank()) return
        registerDevice()
    }

    private suspend fun registerDevice() {
        val origin = serverOrigin(officialOrigin, BuildConfig.DEBUG)
        val originalCookie =
            vault.read(origin.toString()) ?: error("Connectez-vous à votre compte Leo.")
        val api = LeoApi(origin, vault)
        val session = api.get<Session>("/account/session")
        require(session.authenticated && session.account != null) {
            "Connectez-vous à votre compte Leo."
        }
        api.csrf = session.csrf.orEmpty()
        require(api.get<AndroidPushConfiguration>("/account/notifications/android").enabled) {
            "Le service officiel ne propose pas les notifications Android actuellement."
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
            "La session du compte Leo a changé."
        }
        val registration =
            api.send<RegisteredDevice>(
                "POST",
                "/account/notifications/android",
                body("deviceId" to deviceId, "token" to token),
            )
        require(vault.read(origin.toString()) == originalCookie) {
            "La session du compte Leo a changé."
        }
        context.dataStore.edit {
            it[nativeRegistrationKey] = registration.id
            it[nativeRegistrationScopeKey] = scope
        }
    }

    suspend fun disable() {
        NotificationPreferences(context).setEnabled(false)
        try {
            val stored = context.dataStore.data.first()
            val id = stored[nativeRegistrationKey]
            if (id != null && officialOrigin.isNotBlank()) {
                val api = LeoApi(serverOrigin(officialOrigin, BuildConfig.DEBUG), vault)
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
