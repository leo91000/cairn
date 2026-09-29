package dev.leo.manager.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import dev.leo.manager.data.*
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** The accounts coding agents run on, grouped by coding agent, then the tools agents reach. */
@Composable
fun ConnectionsScreen(vm: LeoViewModel, state: Workspace, openRun: (String) -> Unit = {}) {
    var view by remember { mutableStateOf<AccountsView?>(null) }
    var opened by remember { mutableStateOf<String?>(null) }
    var adding by remember { mutableStateOf<String?>(null) }
    var addingAny by remember { mutableStateOf(false) }
    fun show(next: AccountsView) {
        val previous = view?.signIn
        if (previous?.state == "pending" && next.signIn?.state == "complete") {
            vm.notify(
                "${next.accounts.find { it.id == next.signIn.accountId }?.name ?: "Compte"} connecté"
            )
            addingAny = false
            adding = null
        }
        view = next
    }
    suspend fun load() {
        val next = vm.api.get<AccountsView>("/accounts")
        // The sheets are separate windows: their state changes on the main thread only.
        withContext(Dispatchers.Main) { show(next) }
    }
    val signingIn = view?.signIn?.state == "pending"
    Poll("accounts", if (signingIn) 1500 else 4000) {
        try {
            load()
        } catch (e: Exception) {
            vm.report(e)
        }
    }
    val accounts = view?.accounts.orEmpty()
    Column(
        Modifier.fillMaxSize()
            .verticalScroll(rememberScrollState())
            .testTag("connections")
            .padding(horizontal = 16.dp)
            .padding(top = 4.dp, bottom = 32.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        ScreenTitle(
            "Connexions",
            "Les comptes de vos agents et leurs outils.",
            Modifier.padding(start = 4.dp, bottom = 10.dp),
        ) {
            RoundAction(
                "Ajouter un compte",
                LeoIcons.Plus,
                container = signal.ink,
                content = signal.onInk,
                outlined = false,
                enabled = !state.busy,
            ) {
                vm.clearMessage()
                addingAny = true
            }
        }
        Row(Modifier.padding(start = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Eyebrow("Agents de code", Modifier.weight(1f))
            Text(
                "Le plus de capacité passe en premier",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            IconButton(
                onClick = {
                    vm.perform {
                        val next = api.send<AccountsView>("POST", "/accounts/refresh")
                        withContext(Dispatchers.Main) { show(next) }
                    }
                },
                enabled = !state.busy && view != null,
                modifier = Modifier.size(36.dp),
            ) {
                Icon(
                    LeoIcons.Retry,
                    "Vérifier l’usage maintenant",
                    Modifier.size(16.dp),
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        if (view == null) LinearProgressIndicator(Modifier.fillMaxWidth())
        else
            codingAgents.forEach { agent ->
                val provider = agent.provider
                CodingAgentCard(
                    agent,
                    accounts.filter { it.provider == provider },
                    provider in view?.required.orEmpty(),
                    add = {
                        vm.clearMessage()
                        adding = provider
                    },
                ) {
                    opened = it.id
                }
            }
        Eyebrow("Outils", Modifier.padding(start = 4.dp, top = 14.dp))
        SignalCard(Modifier.fillMaxWidth(), padding = PaddingValues(0.dp)) {
            GithubConnection(vm, state)
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            OnePasswordAccounts(vm, state)
        }
    }
    accounts
        .find { it.id == opened }
        ?.let { account ->
            AccountSheet(
                vm,
                state,
                account,
                openRun,
                reconnect = {
                    opened = null
                    vm.perform {
                        api.request("POST", "/accounts/${segment(account.id)}/sign-in")
                        load()
                    }
                },
                changed = { load() },
                close = { opened = null },
            )
        }
    // A sign-in that is still pending or failed reopens its sheet, including after a restart.
    val flow = view?.signIn?.takeIf { it.state == "pending" || it.state == "failed" }
    if (addingAny || adding != null || flow != null)
        AccountSignInSheet(vm, state, flow, adding, changed = { load() }) {
            addingAny = false
            adding = null
        }
}

/** GitHub through the server's `gh` login, with its device-code sign-in. */
@Composable
private fun GithubConnection(vm: LeoViewModel, state: Workspace) {
    var github by remember { mutableStateOf<Connection?>(null) }
    var flow by remember { mutableStateOf<DeviceFlow?>(null) }
    suspend fun load(force: Boolean = false) {
        github =
            vm.api.get<List<Connection>>("/connections" + if (force) "?refresh=true" else "").find {
                it.provider == "github"
            }
    }
    Poll("github", 3000) {
        try {
            if (github == null) load(true)
            if (flow?.state == "pending") {
                val next = vm.api.get<DeviceFlow?>("/connections/login")
                val completed = next?.state == "complete"
                flow = next
                if (completed) load(true)
            }
        } catch (e: Exception) {
            vm.report(e)
        }
    }
    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(
                Modifier.size(36.dp)
                    .clip(RoundedCornerShape(11.dp))
                    .background(MaterialTheme.colorScheme.surfaceVariant),
                contentAlignment = Alignment.Center,
            ) {
                Icon(LeoIcons.Plug, null, Modifier.size(18.dp))
            }
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text("GitHub", style = MaterialTheme.typography.titleSmall)
                Text(
                    when {
                        github == null -> "…"
                        github?.connected == true -> github?.account ?: "Connecté"
                        github?.installed == false -> "L’outil gh n’est pas installé sur le serveur"
                        else -> "Dépôts, pull requests et versions"
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (github?.connected == true)
                Text(
                    "Connecté",
                    style = MaterialTheme.typography.labelMedium,
                    color = signal.success,
                )
            else
                TextButton(
                    onClick = {
                        vm.perform {
                            flow =
                                api.send("POST", "/connections/login", body("provider" to "github"))
                        }
                    },
                    enabled = !state.busy && github?.installed == true && flow?.state != "pending",
                ) {
                    Text("Connecter")
                }
        }
        flow
            ?.takeIf { it.state != "complete" }
            ?.let { current ->
                Surface(
                    shape = RoundedCornerShape(16.dp),
                    color = MaterialTheme.colorScheme.surfaceContainer,
                ) {
                    Column(
                        Modifier.fillMaxWidth().padding(14.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        if (current.state == "failed")
                            Text(
                                current.error ?: "Connexion échouée",
                                color = MaterialTheme.colorScheme.error,
                            )
                        else {
                            Text(
                                "Ouvrez GitHub et saisissez ce code.",
                                style = MaterialTheme.typography.bodyMedium,
                            )
                            current.code
                                ?.takeIf { it.isNotBlank() }
                                ?.let {
                                    Code(it)
                                    CopyButton("Copier le code", it)
                                }
                                ?: Text(
                                    "Le serveur prépare le code de connexion…",
                                    style = MaterialTheme.typography.bodySmall,
                                )
                            current.url
                                ?.takeIf { it.isNotBlank() }
                                ?.let { ExternalButton("Ouvrir GitHub", it) }
                            TextButton(
                                onClick = {
                                    vm.perform {
                                        api.request("DELETE", "/connections/login")
                                        flow = null
                                    }
                                },
                                enabled = !state.busy,
                            ) {
                                Text("Annuler la connexion")
                            }
                        }
                    }
                }
            }
    }
}
