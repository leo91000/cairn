package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.add
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray

/** The accounts coding agents run on, grouped by coding agent. */
@Composable
fun ConnectionsScreen(vm: CairnViewModel, state: Workspace, openRun: (String) -> Unit = {}) {
    var view by remember { mutableStateOf<AccountsView?>(null) }
    var opened by remember { mutableStateOf<String?>(null) }
    var adding by remember { mutableStateOf<String?>(null) }
    var addingAny by remember { mutableStateOf(false) }
    var replacing by remember { mutableStateOf<Account?>(null) }
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
            "Les comptes sur lesquels vos agents s’exécutent.",
            Modifier.padding(start = 4.dp, bottom = 10.dp),
        ) {
            RoundAction(
                "Ajouter un compte",
                CairnIcons.Plus,
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
                    CairnIcons.Retry,
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
                    reorder = { ids ->
                        vm.perform {
                            val next =
                                api.send<AccountsView>(
                                    "POST",
                                    "/accounts/order",
                                    buildJsonObject {
                                        put("provider", provider)
                                        putJsonArray("ids") { ids.forEach { add(it) } }
                                    },
                                )
                            withContext(Dispatchers.Main) { show(next) }
                        }
                    },
                ) {
                    opened = it.id
                }
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
                    if (codingAgent(account.provider).apiKeys) {
                        replacing = account
                        adding = account.provider
                    } else {
                        vm.perform {
                            api.request("POST", "/accounts/${segment(account.id)}/sign-in")
                            load()
                        }
                    }
                },
                changed = { load() },
                close = { opened = null },
            )
        }
    // A sign-in that is still pending or failed reopens its sheet, including after a restart.
    val flow = view?.signIn?.takeIf { it.state == "pending" || it.state == "failed" }
    if (addingAny || adding != null || flow != null)
        AccountSignInSheet(vm, state, flow, adding, account = replacing, changed = { load() }) {
            addingAny = false
            adding = null
            replacing = null
        }
}
