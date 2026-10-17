package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import java.net.URI
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

internal data class PluginAuthorizationActivity(
    val busy: Boolean = false,
    val pending: Boolean = false,
)

/** Explicit scopes and browser consent; no sign-in state is saved across screen destruction. */
@Composable
internal fun PluginAuthorization(
    vm: CairnViewModel,
    path: String,
    title: String,
    scopes: List<String>,
    initial: PluginOAuthStatus,
    enabled: Boolean,
    setupRevision: String? = null,
    directOrigin: String? = null,
    blocked: Boolean = false,
    stateChanged: (PluginAuthorizationActivity) -> Unit = {},
    changed: () -> Unit,
) {
    var authorization by remember { mutableStateOf(initial) }
    var selected by remember {
        mutableStateOf((initial.requestedScopes.ifEmpty { initial.scopes }).filter { it in scopes })
    }
    var consent by remember { mutableStateOf<PluginOAuthConsent?>(null) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var notice by remember { mutableStateOf<String?>(null) }
    var disconnecting by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    LaunchedEffect(busy, disconnecting, consent != null) {
        stateChanged(PluginAuthorizationActivity(busy || disconnecting, consent != null))
    }
    DisposableEffect(Unit) {
        onDispose { stateChanged(PluginAuthorizationActivity()) }
    }
    LaunchedEffect(initial) { authorization = initial }
    LaunchedEffect(scopes) { selected = selected.filter { it in scopes } }
    LaunchedEffect(consent) {
        val pending = consent ?: return@LaunchedEffect
        delay((pending.expiresAt - System.currentTimeMillis()).coerceAtLeast(0))
        consent = null
        notice = "L’autorisation a expiré. Recommencez la connexion."
    }
    fun action(operation: suspend () -> Unit) {
        if (busy || blocked) return
        busy = true
        error = null
        notice = null
        scope.launch {
            try {
                operation()
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                error = e.message
                vm.report(e)
            } finally {
                busy = false
            }
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            when (authorization.status) {
                "authorized" ->
                    authorization.subject?.let { "Connecté en tant que $it" } ?: "Connecté"
                "reconnect" -> "Reconnectez-vous pour continuer"
                else -> "Non connecté"
            }
        )
        error?.let { FormError(it) }
        notice?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        if (!enabled)
            Text(
                "Cette ressource est en pause ou indisponible. Vous pouvez toujours déconnecter votre autorisation enregistrée.",
                style = MaterialTheme.typography.bodySmall,
            )
        Text("Permissions à autoriser", style = MaterialTheme.typography.labelLarge)
        scopes.forEach { permission ->
            Row(
                Modifier.fillMaxWidth()
                    .toggleable(
                        value = permission in selected,
                        enabled = enabled && !busy && !blocked && consent == null,
                        role = Role.Checkbox,
                        onValueChange = { checked ->
                            selected = if (checked) selected + permission else selected - permission
                        },
                    ),
                verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
            ) {
                Checkbox(
                    checked = permission in selected,
                    enabled = enabled && !busy && !blocked && consent == null,
                    onCheckedChange = null,
                )
                Text(permission, Modifier.weight(1f))
            }
        }
        if (authorization.scopes.isNotEmpty())
            Text(
                "Autorisées : ${authorization.scopes.joinToString(", ")}",
                style = MaterialTheme.typography.bodySmall,
            )
        val pending = consent
        if (pending != null) {
            TextButton(
                onClick = { action { browse(context, pending.url) } },
                enabled = !busy && !blocked,
            ) {
                Text("Ouvrir la page d’autorisation")
            }
            pending.redirectUri?.let { callback ->
                SelectionContainer {
                    Text("URL de retour : $callback", style = MaterialTheme.typography.bodySmall)
                }
            }
            Text(
                "Revenez ici après avoir autorisé l’accès. Si vous quittez cet écran, recommencez la connexion.",
                style = MaterialTheme.typography.bodySmall,
            )
            SignalButton("Vérifier l’autorisation", enabled = !busy && !blocked) {
                action {
                    try {
                        val result =
                            vm.api.send<PluginOAuthStatus>(
                                "POST",
                                "$path/finish",
                                body("state" to pending.state),
                            )
                        if (result.status == "pending")
                            notice = "Terminez l’autorisation dans votre navigateur."
                        else {
                            consent = null
                            if (result.status == "authorized") authorization = result
                            else notice = "L’autorisation a été refusée."
                            changed()
                        }
                    } catch (e: Exception) {
                        // Never retry a potentially consumed authorization code automatically.
                        consent = null
                        throw e
                    }
                }
            }
        } else
            SignalButton(
                if (authorization.status in listOf("authorized", "reconnect")) "Reconnecter"
                else "Connecter",
                enabled = enabled && !busy && !blocked && selected.isNotEmpty(),
            ) {
                action {
                    val result =
                        vm.api.send<PluginOAuthConsent>(
                            "POST",
                            path,
                            buildJsonObject {
                                put("scopes", JsonArray(selected.map(::JsonPrimitive)))
                                setupRevision?.let { put("setupRevision", it) }
                                directOrigin
                                    ?.trim()
                                    ?.takeIf { it.isNotEmpty() }
                                    ?.let {
                                        put("directOrigin", it)
                                    }
                            },
                        )
                    val uri = URI(result.url)
                    require(
                        uri.scheme == "https" &&
                            !uri.host.isNullOrBlank() &&
                            uri.rawUserInfo == null
                    ) {
                        "Le fournisseur a renvoyé un lien d’autorisation invalide."
                    }
                    consent = result
                }
            }
        if (consent != null || authorization.status != "disconnected")
            TextButton(
                onClick = { disconnecting = true },
                enabled = !busy && !blocked,
            ) {
                Text("Déconnecter")
            }
    }
    if (disconnecting)
        Confirm(
            "Déconnecter $title ?",
            "Supprime l’autorisation de Cairn et annule les connexions en attente. Pour révoquer l’accès chez le fournisseur, utilisez ses paramètres.",
            busy || blocked,
            error,
            { disconnecting = false },
        ) {
            action {
                vm.api.send<JsonObject>(
                    "DELETE",
                    path,
                    buildJsonObject { setupRevision?.let { put("setupRevision", it) } },
                )
                consent = null
                authorization = PluginOAuthStatus()
                disconnecting = false
                changed()
            }
        }
}
