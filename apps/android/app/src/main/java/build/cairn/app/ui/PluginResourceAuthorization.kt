package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.coroutines.CancellationException

/** Only the owner resource namespace is used here; personal accounts have their own directory. */
@Composable
internal fun PluginResourceAuthorization(
    vm: CairnViewModel,
    path: String,
    provider: PluginOAuthProvider,
    setupRevision: String?,
    enabled: Boolean,
    opened: Boolean,
    open: () -> Unit,
    blocked: Boolean = false,
    stateChanged: (PluginAuthorizationActivity) -> Unit,
    changed: suspend () -> Unit,
) {
    var authorization by remember { mutableStateOf<PluginOAuthStatus?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var loading by remember { mutableStateOf(false) }
    var activity by remember { mutableStateOf(PluginAuthorizationActivity()) }
    var directOrigin by remember { mutableStateOf("") }
    var reload by remember { mutableIntStateOf(0) }
    var refreshResource by remember { mutableStateOf(false) }
    LaunchedEffect(loading, activity) {
        stateChanged(PluginAuthorizationActivity(loading || activity.busy, activity.pending))
    }
    DisposableEffect(Unit) {
        onDispose { stateChanged(PluginAuthorizationActivity()) }
    }
    LaunchedEffect(opened, reload) {
        if (!opened) return@LaunchedEffect
        loading = true
        error = null
        try {
            if (refreshResource) {
                refreshResource = false
                changed()
            }
            authorization = vm.api.get<PluginOAuthStatus>(path)
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            error = e.message
            vm.report(e)
        } finally {
            loading = false
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        HorizontalDivider()
        Text(provider.title, style = MaterialTheme.typography.titleSmall)
        Text(
            "Autorisation partagée de la ressource. Elle ne donne aucun accès aux agents.",
            style = MaterialTheme.typography.bodySmall,
        )
        if (blocked)
            Text(
                "Une autre autorisation est en cours pour cette ressource.",
                style = MaterialTheme.typography.bodySmall,
            )
        error?.let { FormError(it) }
        TextButton(
            onClick = { if (opened) reload++ else open() },
            enabled = !loading && !blocked && !activity.busy && !activity.pending,
        ) {
            Text(
                when {
                    loading -> "Vérification de l’autorisation…"
                    opened -> "Actualiser l’autorisation"
                    else -> "Gérer l’autorisation"
                }
            )
        }
        authorization?.let { initial ->
            Field(
                "Origine HTTPS directe (facultative)",
                directOrigin,
                { directOrigin = it },
                enabled = enabled && !loading && !blocked && !activity.busy && !activity.pending,
            )
            Text(
                "Laissez vide pour utiliser le retour configuré pour cette instance (Beacon par défaut). Sinon, indiquez l’origine HTTPS que vous exposez.",
                style = MaterialTheme.typography.bodySmall,
            )
            PluginAuthorization(
                vm,
                path,
                provider.title,
                provider.scopes,
                initial,
                enabled && !loading,
                setupRevision,
                directOrigin,
                blocked = blocked,
                stateChanged = { activity = it },
            ) {
                refreshResource = true
                reload++
            }
        }
    }
}
