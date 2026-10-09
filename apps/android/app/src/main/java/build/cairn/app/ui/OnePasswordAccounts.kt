package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*

@Serializable
data class OnePasswordAccount(
    val id: String = "",
    val name: String = "",
    val enabled: Boolean = true,
    val agentIds: List<String> = emptyList(),
)

@Composable
fun OnePasswordAccounts(vm: CairnViewModel, state: Workspace) {
    var accounts by remember { mutableStateOf<List<OnePasswordAccount>>(emptyList()) }
    var loaded by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var notice by remember { mutableStateOf<String?>(null) }
    var editing by remember { mutableStateOf<OnePasswordAccount?>(null) }
    var removing by remember { mutableStateOf<OnePasswordAccount?>(null) }
    suspend fun load() {
        accounts = vm.api.get("/onepassword")
        loaded = true
        error = null
    }
    LaunchedEffect(Unit) {
        try {
            load()
        } catch (e: Exception) {
            error = e.message
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
                Icon(CairnIcons.Key, null, Modifier.size(18.dp))
            }
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text("1Password", style = MaterialTheme.typography.titleSmall)
                val agents =
                    accounts
                        .filter { it.enabled }
                        .flatMap { it.agentIds }
                        .filter { id -> state.agents.any { it.id == id } }
                        .distinct()
                        .size
                Text(
                    when {
                        !loaded -> "…"
                        accounts.isEmpty() -> "Aucun compte de service"
                        else ->
                            "${accounts.size} compte${if (accounts.size > 1) "s" else ""} · partagé avec $agents agent${if (agents > 1) "s" else ""}"
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            TextButton(
                onClick = {
                    vm.clearMessage()
                    editing = OnePasswordAccount()
                },
                enabled = loaded && !state.busy,
            ) {
                Text("Ajouter")
            }
        }
        if (!loaded && error != null)
            TextButton(onClick = { vm.perform { load() } }, enabled = !state.busy) {
                Text("Réessayer 1Password")
            }
        error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        notice?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        accounts.forEach { account ->
            Surface(
                shape = RoundedCornerShape(14.dp),
                color = MaterialTheme.colorScheme.surfaceContainer,
            ) {
                Column(
                    Modifier.fillMaxWidth()
                        .padding(start = 14.dp, end = 4.dp, top = 10.dp, bottom = 4.dp)
                ) {
                    Text(account.name, style = MaterialTheme.typography.titleSmall)
                    Text(
                        "${if (account.enabled) "Activé" else "Désactivé"} · ${account.agentIds.count { id -> state.agents.any { it.id == id } }} agents autorisés",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Row {
                        TextButton(
                            onClick = {
                                vm.clearMessage()
                                editing = account
                            },
                            enabled = !state.busy,
                        ) {
                            Text("Modifier")
                        }
                        TextButton(
                            onClick = {
                                notice = null
                                vm.perform {
                                    api.request("POST", "/onepassword/${segment(account.id)}/test")
                                    notice = "${account.name} : connexion vérifiée"
                                }
                            },
                            enabled = !state.busy,
                        ) {
                            Text("Tester")
                        }
                        TextButton(
                            onClick = {
                                vm.clearMessage()
                                removing = account
                            },
                            enabled = !state.busy,
                        ) {
                            Text("Supprimer")
                        }
                    }
                }
            }
        }
    }
    editing?.let { initial ->
        OnePasswordEditor(vm, state, initial, close = { editing = null }) {
            editing = null
            load()
        }
    }
    removing?.let { account ->
        Confirm(
            "Supprimer ${account.name} ?",
            "Le token sera supprimé et tous les agents perdront l’accès.",
            state.busy,
            state.error,
            dismiss = { removing = null },
            action = {
                vm.perform {
                    api.request("DELETE", "/onepassword/${segment(account.id)}")
                    removing = null
                    load()
                }
            },
        )
    }
}

@Composable
fun OnePasswordEditor(
    vm: CairnViewModel,
    state: Workspace,
    initial: OnePasswordAccount,
    close: () -> Unit,
    saved: suspend () -> Unit,
) {
    var name by remember(initial.id) { mutableStateOf(initial.name) }
    // Deliberately not saveable: never persist a token in saved instance state.
    var token by remember(initial.id) { mutableStateOf("") }
    var enabled by remember(initial.id) { mutableStateOf(initial.enabled) }
    var agents by
        remember(initial.id) {
            mutableStateOf(initial.agentIds.filter { id -> state.agents.any { it.id == id } })
        }
    Editor(
        if (initial.id.isBlank()) "Ajouter un compte 1Password" else "Modifier le compte 1Password",
        state.busy,
        state.error,
        close = close,
        valid =
            name.isNotBlank() &&
                name.length <= 100 &&
                (initial.id.isNotBlank() || token.isNotBlank()),
        save = {
            vm.perform {
                api.request(
                    if (initial.id.isBlank()) "POST" else "PUT",
                    "/onepassword" + if (initial.id.isBlank()) "" else "/${segment(initial.id)}",
                    buildJsonObject {
                        put("name", name.trim())
                        put("enabled", enabled)
                        put("agentIds", JsonArray(agents.map(::JsonPrimitive)))
                        if (token.isNotBlank()) put("token", token.trim())
                    },
                )
                token = ""
                saved()
            }
        },
    ) {
        Field("Nom", name, { name = it }, enabled = !state.busy)
        OutlinedTextField(
            token,
            { token = it },
            Modifier.fillMaxWidth(),
            label = { Text("Token de compte de service") },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            keyboardOptions = InputKeyboards.Password,
            enabled = !state.busy,
        )
        Text(
            if (initial.id.isBlank()) "Token ops_… chiffré sur le serveur, jamais réaffiché."
            else "Laissez vide pour conserver le token enregistré."
        )
        Text(
            "Les agents disposent uniquement de la lecture. Les coffres accessibles dépendent des permissions du compte dans 1Password."
        )
        Toggle("Activer ce compte", enabled) { if (!state.busy) enabled = it }
        Text("Agents autorisés", style = MaterialTheme.typography.titleMedium)
        Text(
            "Aucun agent n’a accès par défaut, y compris l’agent principal. Retirer un agent bloque immédiatement ses prochaines lectures. Les secrets déjà récupérés ne peuvent pas être rappelés."
        )
        state.agents.forEach { agent ->
            Toggle(agent.name, agent.id in agents) { allowed ->
                if (!state.busy) agents = if (allowed) agents + agent.id else agents - agent.id
            }
        }
    }
}
