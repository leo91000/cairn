package dev.leo.manager.data

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map

internal val Context.dataStore by preferencesDataStore("leo_preferences")

class Preferences(private val context: Context) {
    private val themeKey = stringPreferencesKey("theme")
    val theme = context.dataStore.data.map { it[themeKey] ?: "system" }

    suspend fun setTheme(value: String) {
        require(value in listOf("system", "light", "dark"))
        context.dataStore.edit { it[themeKey] = value }
    }

    private fun installationKey(origin: String, account: String) =
        stringPreferencesKey("installation:$origin:$account")

    suspend fun lastInstallation(origin: String, account: String): String? =
        context.dataStore.data.first()[installationKey(origin, account)]

    suspend fun selectInstallation(origin: String, account: String, id: String) {
        context.dataStore.edit { it[installationKey(origin, account)] = id }
    }
}

/** Only the session cookie is persisted, encrypted with a non-exportable device key. */
class KeystoreSessionVault(context: Context) : SessionVault {
    private val file = File(context.noBackupFilesDir, "leo-session")
    private val alias = "leo.session.v1"

    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        return (store.getKey(alias, null) as? SecretKey)
            ?: KeyGenerator.getInstance(
                    KeyProperties.KEY_ALGORITHM_AES,
                    "AndroidKeyStore",
                )
                .apply {
                    init(
                        KeyGenParameterSpec.Builder(
                                alias,
                                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
                            )
                            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                            .build()
                    )
                }
                .generateKey()
    }

    @Synchronized
    override fun read(origin: String): String? = runCatching {
        if (!file.exists()) return null
        val parts = file.readText().split('\n')
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            Cipher.DECRYPT_MODE,
            key(),
            GCMParameterSpec(128, Base64.decode(parts[0], Base64.NO_WRAP)),
        )
        cipher.updateAAD(origin.toByteArray())
        String(cipher.doFinal(Base64.decode(parts[1], Base64.NO_WRAP)), Charsets.UTF_8)
    }
        .getOrNull()

    @Synchronized
    override fun write(origin: String, cookie: String?) {
        if (cookie == null) {
            file.delete()
            return
        }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key())
        cipher.updateAAD(origin.toByteArray())
        val data =
            Base64.encodeToString(cipher.iv, Base64.NO_WRAP) +
                "\n" +
                Base64.encodeToString(cipher.doFinal(cookie.toByteArray()), Base64.NO_WRAP)
        val atomic = android.util.AtomicFile(file)
        val output = atomic.startWrite()
        try {
            output.write(data.toByteArray())
            atomic.finishWrite(output)
        } catch (e: Exception) {
            atomic.failWrite(output)
            throw e
        }
    }
}
