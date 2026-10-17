package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
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

/** Personal connections are available to members without reading owner configuration. */
@Composable
fun PersonalPluginAccountsScreen(vm: CairnViewModel, state: Workspace) {
    key(state.installation?.id, state.session.account?.id) {
        PersonalPluginAccounts(vm)
    }
}

@Composable
private fun PersonalPluginAccounts(vm: CairnViewModel) {
    var page by remember { mutableStateOf(PluginPage<PluginPersonalAuthorization>()) }
    var loaded by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(true) }
    var error by remember { mutableStateOf<String?>(null) }
    var loadingMore by remember { mutableStateOf(false) }
    var reload by remember { mutableIntStateOf(0) }
    LaunchedEffect(reload) {
        busy = true
        error = null
        try {
            val cursor = page.nextCursor.takeIf { loadingMore }
            val next =
                vm.api.get<PluginPage<PluginPersonalAuthorization>>(
                    "/plugins/personal-authorizations" +
                        (cursor?.let { "?cursor=${segment(it)}" } ?: "")
                )
            page =
                if (cursor == null) next else PluginPage(page.items + next.items, next.nextCursor)
            loaded = true
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            page = PluginPage()
            error = e.message
            vm.report(e)
        } finally {
            busy = false
        }
    }
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        ScreenTitle(
            "Vos comptes de plugins",
            "Connexions personnelles de cette installation. Chaque demande utilise l’autorisation de son auteur.",
        )
        Text(
            "Se connecter ne donne aucun accès aux agents. Le propriétaire doit autoriser séparément leurs outils et leurs ressources.",
            style = MaterialTheme.typography.bodySmall,
        )
        TextButton(
            enabled = !busy,
            onClick = {
                loadingMore = false
                reload++
            },
        ) {
            Text("Actualiser")
        }
        if (busy) LinearProgressIndicator(Modifier.fillMaxWidth())
        error?.let { FormError(it) }
        if (loaded && page.items.isEmpty())
            Text(
                "Aucune connexion personnelle disponible. Le propriétaire doit d’abord configurer une ressource de plugin."
            )
        page.items.forEach { item ->
            key(item.id) {
                SignalCard(Modifier.fillMaxWidth()) {
                    Text(item.resourceName, style = MaterialTheme.typography.titleMedium)
                    Text(
                        "${item.pluginName} · ${item.instanceName} · ${item.title}",
                        style = MaterialTheme.typography.bodySmall,
                    )
                    PluginAuthorization(
                        vm,
                        item.path,
                        item.title,
                        item.scopes,
                        item.authorization,
                        item.enabled,
                    ) {
                        loadingMore = false
                        reload++
                    }
                }
            }
        }
        if (page.nextCursor != null)
            TextButton(
                enabled = !busy,
                onClick = {
                    loadingMore = true
                    reload++
                },
            ) {
                Text("Plus de connexions personnelles")
            }
    }
}

/** Explicit scopes and browser consent; no sign-in state is saved across screen destruction. */
@Composable
internal fun PluginAuthorization(
    vm: CairnViewModel,
    path: String,
    title: String,
    scopes: List<String>,
    initial: PluginOAuthStatus,
    enabled: Boolean,
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
    LaunchedEffect(initial) { authorization = initial }
    LaunchedEffect(scopes) { selected = selected.filter { it in scopes } }
    LaunchedEffect(consent) {
        val pending = consent ?: return@LaunchedEffect
        delay((pending.expiresAt - System.currentTimeMillis()).coerceAtLeast(0))
        consent = null
        notice = "L’autorisation a expiré. Recommencez la connexion."
    }
    fun action(operation: suspend () -> Unit) {
        if (busy) return
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
                        enabled = enabled && !busy && consent == null,
                        role = Role.Checkbox,
                        onValueChange = { checked ->
                            selected = if (checked) selected + permission else selected - permission
                        },
                    ),
                verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
            ) {
                Checkbox(
                    checked = permission in selected,
                    enabled = enabled && !busy && consent == null,
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
            TextButton(onClick = { action { browse(context, pending.url) } }, enabled = !busy) {
                Text("Ouvrir la page d’autorisation")
            }
            Text(
                "Revenez ici après avoir autorisé l’accès. Si vous quittez cet écran, recommencez la connexion.",
                style = MaterialTheme.typography.bodySmall,
            )
            SignalButton("Vérifier l’autorisation", enabled = !busy) {
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
                enabled = enabled && !busy && selected.isNotEmpty(),
            ) {
                action {
                    val result =
                        vm.api.send<PluginOAuthConsent>(
                            "POST",
                            path,
                            buildJsonObject {
                                put("scopes", JsonArray(selected.map(::JsonPrimitive)))
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
                enabled = !busy,
            ) {
                Text("Déconnecter")
            }
    }
    if (disconnecting)
        Confirm(
            "Déconnecter $title ?",
            "Supprime l’autorisation de Cairn et annule les connexions en attente. Pour révoquer l’accès chez le fournisseur, utilisez ses paramètres.",
            busy,
            error,
            { disconnecting = false },
        ) {
            action {
                vm.api.send<JsonObject>("DELETE", path)
                consent = null
                authorization = PluginOAuthStatus()
                disconnecting = false
                changed()
            }
        }
}
