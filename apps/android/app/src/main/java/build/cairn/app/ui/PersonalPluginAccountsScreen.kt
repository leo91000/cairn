package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.coroutines.CancellationException

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
