package dev.leo.manager.data

import android.content.Context
import android.os.Build
import androidx.work.*
import com.google.firebase.FirebaseApp
import com.google.firebase.FirebaseOptions
import com.google.firebase.messaging.FirebaseMessaging
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import dev.leo.manager.BuildConfig
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeout

fun initializeNativePush(context: Context) {
    if (BuildConfig.FIREBASE_APP_ID.isBlank()) return
    if (FirebaseApp.getApps(context).isNotEmpty()) return
    FirebaseApp.initializeApp(
        context,
        FirebaseOptions.Builder()
            .setApplicationId(BuildConfig.FIREBASE_APP_ID)
            .setProjectId(BuildConfig.FIREBASE_PROJECT_ID)
            .setApiKey(BuildConfig.FIREBASE_API_KEY)
            .setGcmSenderId(BuildConfig.FIREBASE_SENDER_ID)
            .build(),
    )
}

class FirebasePushTokens : PushTokens {
    override val available: Boolean
        get() = runCatching { FirebaseApp.getInstance() }.isSuccess

    override suspend fun token(): String =
        withTimeout(15_000) {
            FirebaseMessaging.getInstance().isAutoInitEnabled = true
            suspendCancellableCoroutine { continuation ->
                FirebaseMessaging.getInstance().token.addOnCompleteListener { task ->
                    if (!continuation.isActive) return@addOnCompleteListener
                    if (task.isSuccessful) continuation.resume(task.result)
                    else
                        continuation.resumeWithException(
                            IllegalStateException(
                                "Le service de notifications est indisponible. Réessayez."
                            )
                        )
                }
            }
        }

    override suspend fun delete() {
        FirebaseMessaging.getInstance().isAutoInitEnabled = false
        withTimeout(15_000) {
            suspendCancellableCoroutine<Unit> { continuation ->
                FirebaseMessaging.getInstance().deleteToken().addOnCompleteListener { task ->
                    if (!continuation.isActive) return@addOnCompleteListener
                    if (task.isSuccessful) continuation.resume(Unit)
                    else
                        continuation.resumeWithException(
                            IllegalStateException(
                                "La désactivation sera effective sur cet appareil."
                            )
                        )
                }
            }
        }
    }
}

class LeoMessagingService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        enqueueNativeDevice(applicationContext)
    }

    override fun onMessageReceived(message: RemoteMessage) {
        val identifiers =
            message.data.filterKeys {
                it in setOf("accountId", "installationId", "chatId", "questionId", "alertId")
            }
        val request =
            OneTimeWorkRequestBuilder<NativeMessageWorker>()
                .setInputData(
                    Data.Builder()
                        .putAll(identifiers)
                        .putLong("receivedAt", System.currentTimeMillis())
                        .build()
                )
                .setConstraints(
                    Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build()
                )
        if (Build.VERSION.SDK_INT >= 31)
            request.setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
        val receipt =
            java.security.MessageDigest.getInstance("SHA-256")
                .digest(identifiers.toSortedMap().toString().toByteArray())
                .joinToString("") { "%02x".format(it) }
        WorkManager.getInstance(applicationContext)
            .enqueueUniqueWork("leo-native-$receipt", ExistingWorkPolicy.KEEP, request.build())
    }
}

fun enqueueNativeDevice(context: Context) {
    WorkManager.getInstance(context)
        .enqueueUniqueWork(
            "leo-native-device",
            ExistingWorkPolicy.REPLACE,
            OneTimeWorkRequestBuilder<NativeDeviceWorker>()
                .setConstraints(
                    Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build()
                )
                .build(),
        )
}

class NativeDeviceWorker(context: Context, parameters: WorkerParameters) :
    CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result =
        try {
            NativeDeviceRegistrar(applicationContext).register()
            Result.success()
        } catch (error: CancellationException) {
            throw error
        } catch (error: ApiException) {
            if (error.status in setOf(401, 403)) Result.success() else Result.retry()
        } catch (_: Exception) {
            Result.retry()
        }
}

class NativeMessageWorker(context: Context, parameters: WorkerParameters) :
    CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result {
        val age = System.currentTimeMillis() - inputData.getLong("receivedAt", 0)
        if (age !in 0..3_600_000) return Result.success()
        return try {
            val identifiers =
                inputData.keyValueMap
                    .filterKeys { it != "receivedAt" }
                    .mapValues { it.value.toString() }
            NativeNotificationReceiver(applicationContext).receive(identifiers)
            Result.success()
        } catch (error: CancellationException) {
            throw error
        } catch (error: ApiException) {
            if (error.status in setOf(401, 403, 404)) Result.success() else Result.retry()
        } catch (_: Exception) {
            Result.retry()
        }
    }
}
