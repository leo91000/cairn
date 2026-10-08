package dev.leo.manager.ui

import android.Manifest
import android.content.Intent
import android.os.Build
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.leo.manager.data.*

@Composable
fun NotificationSettings(vm: LeoViewModel) {
    val context = LocalContext.current
    val enabled by vm.notifications.enabled.collectAsStateWithLifecycle(false)
    val reenrollmentRequired by
        vm.notifications.nativeReenrollmentRequired.collectAsStateWithLifecycle(false)
    var allowed by remember { mutableStateOf(notificationsAllowed(context)) }
    Poll("notification-permission", 3000) { allowed = notificationsAllowed(context) }
    val permission =
        rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            allowed = granted && notificationsAllowed(context)
            if (allowed) vm.perform { setNativeNotifications(true) }
        }
    Panel {
        Text("Notifications", style = MaterialTheme.typography.titleLarge)
        Text("Être prévenu lorsqu’un agent attend une réponse.")
        Toggle("Notifications push", enabled) { next ->
            if (next && Build.VERSION.SDK_INT >= 33 && !allowed)
                permission.launch(Manifest.permission.POST_NOTIFICATIONS)
            else vm.perform { setNativeNotifications(next) }
        }
        if (reenrollmentRequired) Text(NATIVE_PUSH_REENROLLMENT_MESSAGE)
        Text(
            "Recevez les notifications de toutes vos installations accessibles avec un seul enregistrement de cet appareil. Android peut retarder leur réception pour économiser la batterie.",
            style = MaterialTheme.typography.bodySmall,
        )
        if (!allowed) Text("Les notifications sont actuellement désactivées dans Android.")
        TextButton(
            onClick = {
                context.startActivity(
                    Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                        .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
                )
            }
        ) {
            Text("Réglages Android")
        }
    }
}
