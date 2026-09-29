package dev.leo.manager.ui

import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.leo.manager.BuildConfig
import dev.leo.manager.update.UpdateViewModel

val LocalAppUpdates = staticCompositionLocalOf<UpdateViewModel?> { null }

@Composable
fun AppUpdatePrompt(vm: UpdateViewModel) {
    val state by vm.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    LifecycleEventEffect(Lifecycle.Event.ON_START) { vm.check() }
    val update = state.available ?: return
    if (!state.showDialog) return
    AlertDialog(
        onDismissRequest = vm::dismiss,
        title = { Text("Mise à jour disponible") },
        text = {
            Column {
                Text("Leo ${update.versionName}")
                Text("L’installation redémarrera l’application. Vos données seront conservées.")
                if (state.downloading) {
                    LinearProgressIndicator(progress = { state.progress / 100f })
                    Text("Téléchargement : ${state.progress} %")
                }
                if (state.ready != null) {
                    Text("La mise à jour est prête à être installée.")
                    if (!context.packageManager.canRequestPackageInstalls())
                        Text(
                            "Autorisez Leo à installer des applications dans les paramètres Android, puis revenez ici et touchez Installer."
                        )
                }
                state.message?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            }
        },
        confirmButton = {
            TextButton(
                enabled = !state.downloading && !state.installing,
                onClick = {
                    if (state.ready == null) vm.download()
                    else if (!context.packageManager.canRequestPackageInstalls()) {
                        try {
                            context.startActivity(
                                Intent(
                                    Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                                    Uri.parse("package:${context.packageName}"),
                                )
                            )
                        } catch (e: Exception) {
                            vm.report(e)
                        }
                    } else vm.install(context::startActivity)
                },
            ) {
                Text(if (state.ready == null) "Télécharger" else "Installer")
            }
        },
        dismissButton = { TextButton(onClick = vm::dismiss) { Text("Plus tard") } },
    )
}

@Composable
fun AppUpdateSettings() {
    val vm = LocalAppUpdates.current ?: return
    val state by vm.state.collectAsStateWithLifecycle()
    Panel {
        Text("Mises à jour", style = MaterialTheme.typography.titleLarge)
        Text("Application Android ${BuildConfig.VERSION_NAME}")
        Text("Les nouvelles versions sont recherchées automatiquement à l’ouverture.")
        if (state.available != null) {
            TextButton(onClick = vm::show) {
                Text("Voir la mise à jour ${state.available!!.versionName}")
            }
        }
        OutlinedButton(
            onClick = { vm.check(manual = true) },
            enabled = !state.checking && !state.downloading,
        ) {
            Text(if (state.checking) "Vérification…" else "Rechercher une mise à jour")
        }
        state.message?.let { Text(it) }
    }
}
