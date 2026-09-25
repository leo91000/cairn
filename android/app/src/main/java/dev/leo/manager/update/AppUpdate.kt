package dev.leo.manager.update

import android.content.Context
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.content.FileProvider
import dev.leo.manager.BuildConfig
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.InputStream
import java.io.OutputStream
import java.security.MessageDigest
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import okhttp3.OkHttpClient
import okhttp3.Request

const val RELEASE_BASE = "https://github.com/leo91000/leo-agent-manager/releases"
const val MAX_APK_BYTES = 100L * 1024 * 1024

@Serializable
data class AppUpdate(
    val schema: Int,
    val versionName: String,
    val versionCode: Int,
    val minSdk: Int,
    val url: String,
    val sha256: String,
    val size: Long,
) {
    fun validate() {
        require(schema == 1 && versionCode > 0 && minSdk >= 26) {
            "Version de mise à jour invalide."
        }
        require(Regex("(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)").matches(versionName))
        require(url == "$RELEASE_BASE/download/v$versionName/leo-android.apk") {
            "Source de mise à jour invalide."
        }
        require(Regex("[a-f0-9]{64}").matches(sha256) && size in 1..MAX_APK_BYTES)
    }

    fun isNewer(installed: Long, sdk: Int): Boolean = versionCode > installed && minSdk <= sdk
}

/** Bound both known and chunked responses; never keep a truncated or corrupt APK. */
internal fun copyVerified(
    input: InputStream,
    output: OutputStream,
    update: AppUpdate,
    progress: (Long) -> Unit,
) {
    val digest = MessageDigest.getInstance("SHA-256")
    val buffer = ByteArray(64 * 1024)
    var total = 0L
    while (true) {
        val count = input.read(buffer)
        if (count == -1) break
        total += count
        require(total <= update.size && total <= MAX_APK_BYTES) {
            "Téléchargement trop volumineux."
        }
        digest.update(buffer, 0, count)
        output.write(buffer, 0, count)
        progress(total)
    }
    require(
        total == update.size &&
            digest.digest().joinToString("") { "%02x".format(it) } == update.sha256
    ) {
        "Le téléchargement est incomplet ou endommagé. Réessayez."
    }
}

// Separate client: no Leo cookies, tokens or server credentials are sent to GitHub.
private fun defaultUpdateHttpClient() =
    OkHttpClient.Builder()
        .followSslRedirects(false)
        .connectTimeout(15, TimeUnit.SECONDS)
        .readTimeout(30, TimeUnit.SECONDS)
        .callTimeout(5, TimeUnit.MINUTES)
        .build()

class AppUpdateClient(
    private val context: Context,
    private val client: OkHttpClient = defaultUpdateHttpClient(),
) {
    private val json = Json { ignoreUnknownKeys = true }

    suspend fun check(): AppUpdate? =
        withContext(Dispatchers.IO) {
            val request =
                Request.Builder().url("$RELEASE_BASE/latest/download/android-update.json").build()
            client.newCall(request).execute().use { response ->
                if (response.code == 404) return@withContext null
                check(response.isSuccessful) {
                    "Impossible de vérifier les mises à jour (${response.code})."
                }
                val bytes =
                    response.body.byteStream().use { input ->
                        val buffer = ByteArray(1024)
                        val output = ByteArrayOutputStream()
                        while (output.size() <= 16 * 1024) {
                            val count = input.read(buffer)
                            if (count == -1) break
                            output.write(buffer, 0, count)
                        }
                        output.toByteArray()
                    }
                require(bytes.size <= 16 * 1024) { "Réponse de mise à jour trop volumineuse." }
                json
                    .decodeFromString<AppUpdate>(bytes.decodeToString())
                    .also { it.validate() }
                    .takeIf { it.isNewer(BuildConfig.VERSION_CODE.toLong(), Build.VERSION.SDK_INT) }
            }
        }

    suspend fun download(update: AppUpdate, progress: (Int) -> Unit): File =
        withContext(Dispatchers.IO) {
            update.validate()
            val directory = File(context.cacheDir, "app-updates").apply { mkdirs() }
            directory.listFiles()?.forEach { it.delete() }
            val partial = File(directory, "update.part")
            val ready = File(directory, "update.apk")
            try {
                client.newCall(Request.Builder().url(update.url).build()).execute().use { response
                    ->
                    check(response.isSuccessful) { "Téléchargement impossible (${response.code})." }
                    val length = response.body.contentLength()
                    require(length == -1L || length == update.size) {
                        "Taille de téléchargement invalide."
                    }
                    partial.outputStream().use { output ->
                        copyVerified(response.body.byteStream(), output, update) { bytes ->
                            coroutineContext.ensureActive()
                            progress((bytes * 100 / update.size).toInt())
                        }
                    }
                }
                verifyApk(partial, update)
                check(partial.renameTo(ready)) { "Impossible de préparer l’installation." }
                ready
            } finally {
                partial.delete()
            }
        }

    @Suppress("DEPRECATION")
    internal fun verifyApk(file: File, update: AppUpdate) {
        val pm = context.packageManager
        val flags =
            if (Build.VERSION.SDK_INT >= 28) PackageManager.GET_SIGNING_CERTIFICATES
            else PackageManager.GET_SIGNATURES
        val archive = requireNotNull(pm.getPackageArchiveInfo(file.path, flags)) { "APK invalide." }
        val installed = pm.getPackageInfo(context.packageName, flags)
        validateApkIdentity(archive, installed, update, Build.VERSION.SDK_INT)
    }

    suspend fun installationIntent(file: File, update: AppUpdate): Intent =
        withContext(Dispatchers.IO) {
            // Recheck after returning from Android's permission screen as well.
            val discard =
                object : OutputStream() {
                    override fun write(value: Int) {}

                    override fun write(buffer: ByteArray, offset: Int, length: Int) {}
                }
            file.inputStream().use { copyVerified(it, discard, update) {} }
            verifyApk(file, update)
            Intent(Intent.ACTION_VIEW).apply {
                setDataAndType(
                    FileProvider.getUriForFile(context, "${context.packageName}.files", file),
                    "application/vnd.android.package-archive",
                )
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
        }
}

@Suppress("DEPRECATION")
private fun signers(info: PackageInfo): Set<String> =
    (if (Build.VERSION.SDK_INT >= 28) info.signingInfo?.apkContentsSigners else info.signatures)
        .orEmpty()
        .map { it.toCharsString() }
        .toSet()

@Suppress("DEPRECATION")
internal fun validateApkIdentity(
    archive: PackageInfo,
    installed: PackageInfo,
    update: AppUpdate,
    sdk: Int,
) {
    val code =
        if (Build.VERSION.SDK_INT >= 28) archive.longVersionCode else archive.versionCode.toLong()
    val installedCode =
        if (Build.VERSION.SDK_INT >= 28) installed.longVersionCode
        else installed.versionCode.toLong()
    require(
        archive.packageName == installed.packageName &&
            code == update.versionCode.toLong() &&
            archive.versionName == update.versionName &&
            code > installedCode &&
            (archive.applicationInfo?.minSdkVersion ?: Int.MAX_VALUE) <= sdk
    ) {
        "Cet APK ne correspond pas à la mise à jour attendue."
    }
    require(signers(installed).isNotEmpty() && signers(archive) == signers(installed)) {
        "La signature de cet APK ne correspond pas à l’application installée."
    }
}
