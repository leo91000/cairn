package build.cairn.app.ui

import android.content.Context
import androidx.browser.customtabs.CustomTabsIntent
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.core.net.toUri
import build.cairn.app.data.*
import kotlinx.serialization.json.*

internal fun browse(context: Context, url: String) {
    val uri = url.toUri()
    require(uri.scheme in listOf("https", "http")) { "Lien non pris en charge." }
    CustomTabsIntent.Builder().build().launchUrl(context, uri)
}

@Composable
fun McpsScreen(vm: CairnViewModel, state: Workspace) {
    var items by remember { mutableStateOf(state.mcps) }
    var query by rememberSaveable { mutableStateOf("") }
    var editing by remember { mutableStateOf<Mcp?>(null) }
    var inspecting by remember { mutableStateOf<Mcp?>(null) }
    var removing by remember { mutableStateOf<Mcp?>(null) }
    var disconnecting by remember { mutableStateOf(false) }
    var oauth by rememberSaveable { mutableStateOf<String?>(null) }
    var oauthNotice by remember { mutableStateOf("") }
    val context = LocalContext.current
    LaunchedEffect(state.mcps) { items = state.mcps }
    Poll("mcps", 3000) {
        try {
            oauth?.let { id ->
                val result = vm.api.send<JsonObject>("POST", "/mcps/${segment(id)}/callback")
                if (result["pending"]?.jsonPrimitive?.booleanOrNull == false) {
                    oauth = null
                    oauthNotice =
                        when (result["result"]?.jsonPrimitive?.contentOrNull) {
                            "connected" -> "Connexion OAuth terminée."
                            "denied" -> "Autorisation annulée."
                            else -> "Autorisation expirée ou échouée. Réessayez la connexion."
                        }
                }
            }
            items = vm.api.get("/mcps")
        } catch (e: Exception) {
            vm.report(e)
        }
    }
    Page {
        Heading("Connexions MCP", "Les outils accessibles à vos agents.")
        Button(
            onClick = {
                vm.clearMessage()
                editing = Mcp()
            }
        ) {
            Text("Ajouter un MCP")
        }
        SearchField("Rechercher", query, { query = it })
        if (oauthNotice.isNotBlank()) Text(oauthNotice)
        if (oauth != null) {
            Text("Terminez l’autorisation dans le navigateur, puis revenez dans Cairn.")
            TextButton(onClick = { oauth = null }) { Text("Annuler cette connexion") }
        }
        items
            .filter { "${it.name} ${it.url} ${it.command}".contains(query, true) }
            .forEach { mcp ->
                Panel {
                    Text(mcp.name, style = MaterialTheme.typography.titleMedium)
                    Code(
                        if (mcp.transport == "http") mcp.url
                        else (listOf(mcp.command) + mcp.args).joinToString(" ")
                    )
                    Text(
                        when (mcp.state) {
                            "connected" -> "Connecté"
                            "needs-auth" -> "Connexion requise"
                            "error" -> "Erreur de connexion"
                            else -> "Non testé"
                        },
                        color = MaterialTheme.colorScheme.primary,
                    )
                    if (mcp.error.isNotBlank())
                        Text(mcp.error, color = MaterialTheme.colorScheme.error)
                    Text(
                        "${mcp.tools.size} outils · ${if (mcp.enabled) "Disponible" else "Désactivé"}"
                    )
                    Row {
                        ActionIcon(
                            "Tester",
                            Icons.Default.Refresh,
                            onClick = {
                                vm.perform {
                                    api.request("POST", "/mcps/${segment(mcp.id)}/test")
                                    items = api.get("/mcps")
                                }
                            },
                            enabled = !state.busy,
                        )
                        ActionIcon(
                            "Modifier",
                            Icons.Default.Edit,
                            onClick = {
                                vm.clearMessage()
                                editing = mcp
                            },
                        )
                        ActionIcon(
                            "Outils",
                            CairnIcons.Tune,
                            onClick = {
                                vm.clearMessage()
                                inspecting = mcp
                            },
                        )
                    }
                    if (mcp.auth == "oauth")
                        Button(
                            onClick = {
                                vm.perform {
                                    val settings = api.get<Settings>("/settings")
                                    if (settings.nativeMcpOauth) {
                                        val result =
                                            api.send<UrlResult>(
                                                "POST",
                                                "/mcps/${segment(mcp.id)}/connect",
                                                buildJsonObject { put("native", true) },
                                            )
                                        oauth = mcp.id
                                        oauthNotice = ""
                                        browse(context, result.url)
                                    } else {
                                        oauthNotice =
                                            "Sur cette version du serveur, terminez la connexion MCP dans l’interface web."
                                        browse(
                                            context,
                                            api.origin
                                                .newBuilder()
                                                .encodedPath(
                                                    "/installations/${segment(checkNotNull(api.installationId))}/mcps"
                                                )
                                                .build()
                                                .toString(),
                                        )
                                    }
                                }
                            },
                            enabled = !state.busy && oauth == null,
                        ) {
                            Text("Connexion OAuth")
                        }
                    Row {
                        TextButton(
                            onClick = {
                                disconnecting = true
                                removing = mcp
                            },
                            enabled = !state.busy,
                        ) {
                            Text("Retirer les identifiants")
                        }
                        ActionIcon(
                            "Supprimer",
                            Icons.Default.Delete,
                            onClick = {
                                disconnecting = false
                                removing = mcp
                            },
                            enabled = !state.busy,
                        )
                    }
                }
            }
        if (items.isEmpty())
            Empty("Aucun serveur MCP", "Ajoutez un serveur distant ou une commande.")
    }
    editing?.let { initial -> McpEditor(vm, state, initial) { editing = null } }
    inspecting?.let { initial -> McpToolsEditor(vm, state, initial) { inspecting = null } }
    removing?.let { mcp ->
        Confirm(
            if (disconnecting) "Retirer les identifiants ?" else "Supprimer ce MCP ?",
            "Les accès et secrets correspondants seront retirés du serveur. Les exécutions actives peuvent perdre l’accès à cette connexion.",
            state.busy,
            state.error,
            { removing = null },
        ) {
            vm.perform {
                api.request(
                    if (disconnecting) "POST" else "DELETE",
                    "/mcps/${segment(mcp.id)}" + if (disconnecting) "/disconnect" else "",
                )
                removing = null
                refresh()
            }
        }
    }
}

private data class EnvRow(val key: String, val value: String = "", val saved: Boolean = false)

@Composable
internal fun McpEditor(vm: CairnViewModel, state: Workspace, initial: Mcp, close: () -> Unit) {
    var form by rememberForm(initial)
    var args by rememberForm(initial.args)
    var token by remember { mutableStateOf("") }
    var secret by remember { mutableStateOf("") }
    var env by remember { mutableStateOf(initial.envKeys.map { EnvRow(it, saved = true) }) }
    Editor(
        if (initial.id.isBlank()) "Ajouter un MCP" else "Modifier le MCP",
        state.busy,
        state.error,
        close,
        save = {
            vm.perform {
                val entries = env.filter { it.key.isNotBlank() }
                require(entries.map { it.key.trim() }.distinct().size == entries.size) {
                    "Les noms des variables doivent être uniques."
                }
                require(entries.all { Regex("[A-Za-z_]\\w*").matches(it.key.trim()) }) {
                    "Un nom de variable contient des caractères invalides."
                }
                val value =
                    form.copy(
                        args = args,
                        auth = if (form.transport == "stdio") "none" else form.auth,
                    )
                val payload =
                    JsonObject(
                        wireJson.encodeToJsonElement(value).jsonObject +
                            buildJsonObject {
                                if (token.isNotBlank()) put("token", token)
                                if (secret.isNotBlank()) put("clientSecret", secret)
                                put(
                                    "env",
                                    wireJson.encodeToJsonElement(
                                        entries
                                            .filter { !it.saved || it.value.isNotEmpty() }
                                            .associate { it.key.trim() to it.value }
                                    ),
                                )
                                put(
                                    "removeEnv",
                                    wireJson.encodeToJsonElement(
                                        initial.envKeys.filter { key ->
                                            entries.none { it.key.trim() == key }
                                        }
                                    ),
                                )
                            }
                    )
                save("mcps", initial.id, payload)
                token = ""
                secret = ""
                env = emptyList()
                close()
            }
        },
        valid =
            form.name.isNotBlank() &&
                form.name.length <= 100 &&
                (if (form.transport == "stdio") form.command.isNotBlank()
                else form.url.startsWith("https://") || form.url.startsWith("http://")),
    ) {
        Field("Nom", form.name, { form = form.copy(name = it) })
        Choice(
            "Transport",
            form.transport,
            listOf("http" to "Serveur HTTP", "stdio" to "Commande sur le serveur"),
        ) {
            form = form.copy(transport = it)
        }
        if (form.transport == "http") {
            Field(
                "Adresse du MCP",
                form.url,
                { form = form.copy(url = it) },
                keyboardOptions = InputKeyboards.Uri,
            )
            Toggle("Autoriser les adresses du réseau privé", form.allowPrivateNetwork) {
                form = form.copy(allowPrivateNetwork = it)
            }
            Choice(
                "Authentification",
                form.auth,
                listOf("none" to "Aucune", "bearer" to "Jeton Bearer", "oauth" to "OAuth"),
            ) {
                form = form.copy(auth = it)
            }
            if (form.auth == "bearer")
                SecretField(
                    if (initial.hasToken) "Nouveau jeton (vide = conserver)" else "Jeton",
                    token,
                ) {
                    token = it
                }
            if (form.auth == "oauth") {
                Field(
                    "Identifiant du client (facultatif)",
                    form.clientId,
                    { form = form.copy(clientId = it) },
                    keyboardOptions = InputKeyboards.Literal,
                )
                SecretField(
                    if (initial.hasClientSecret) "Nouveau secret (vide = conserver)"
                    else "Secret du client (facultatif)",
                    secret,
                ) {
                    secret = it
                }
                Field(
                    "Scopes (facultatifs)",
                    form.scopes,
                    { form = form.copy(scopes = it) },
                    keyboardOptions = InputKeyboards.Literal,
                )
                if (initial.callbackUrl.isNotBlank()) {
                    Text("Adresse de retour")
                    Code(initial.callbackUrl)
                    CopyButton("Copier l’adresse de retour", initial.callbackUrl)
                }
            }
        } else {
            Field(
                "Commande",
                form.command,
                { form = form.copy(command = it) },
                keyboardOptions = InputKeyboards.Literal,
            )
            args.forEachIndexed { index, value ->
                Field(
                    "Argument ${index + 1}",
                    value,
                    { next -> args = args.toMutableList().also { it[index] = next } },
                    3,
                    keyboardOptions = InputKeyboards.Literal,
                )
                TextButton(
                    onClick = { args = args.filterIndexed { position, _ -> position != index } }
                ) {
                    Text("Retirer cet argument")
                }
            }
            TextButton(enabled = args.size < 100, onClick = { args = args + "" }) {
                Text("Ajouter un argument")
            }
            Text("Variables d’environnement", style = MaterialTheme.typography.titleMedium)
            env.forEachIndexed { index, row ->
                Field(
                    "Nom de la variable ${index + 1}",
                    row.key,
                    { value ->
                        env = env.toMutableList().also { it[index] = row.copy(key = value) }
                    },
                    enabled = !row.saved,
                    keyboardOptions = InputKeyboards.Literal,
                )
                SecretField(if (row.saved) "Valeur (vide = conserver)" else "Valeur", row.value) {
                    value ->
                    env = env.toMutableList().also { it[index] = row.copy(value = value) }
                }
                TextButton(onClick = { env = env.filterIndexed { i, _ -> i != index } }) {
                    Text("Retirer ${row.key}")
                }
            }
            OutlinedButton(onClick = { env = env + EnvRow("") }, enabled = env.size < 100) {
                Text("Ajouter une variable")
            }
        }
        Toggle("Disponible pour les agents", form.enabled) { form = form.copy(enabled = it) }
    }
}

@Composable
internal fun SecretField(label: String, value: String, change: (String) -> Unit) {
    OutlinedTextField(
        value,
        change,
        Modifier.fillMaxWidth(),
        label = { Text(label) },
        visualTransformation = PasswordVisualTransformation(),
        keyboardOptions = InputKeyboards.Password,
        singleLine = true,
    )
}

@Composable
private fun McpToolsEditor(vm: CairnViewModel, state: Workspace, initial: Mcp, close: () -> Unit) {
    var selected by rememberForm(initial.enabledTools ?: emptyList())
    var all by rememberSaveable { mutableStateOf(initial.enabledTools == null) }
    var query by rememberSaveable { mutableStateOf("") }
    Editor(
        "${initial.name} · outils",
        state.busy,
        state.error,
        close,
        save = {
            vm.perform {
                save(
                    "mcps",
                    initial.id,
                    wireJson.encodeToJsonElement(
                        initial.copy(enabledTools = if (all) null else selected)
                    ),
                )
                close()
            }
        },
    ) {
        Toggle("Tous les outils, y compris les futurs", all) { all = it }
        Field("Rechercher un outil", query, { query = it }, keyboardOptions = InputKeyboards.Search)
        initial.tools
            .filter { "${it.name} ${it.description}".contains(query, true) }
            .forEach { tool ->
                if (!all)
                    Toggle(tool.title ?: tool.name, tool.name in selected) {
                        selected = if (it) selected + tool.name else selected - tool.name
                    }
                else Text(tool.title ?: tool.name, style = MaterialTheme.typography.titleMedium)
                Text(tool.name, style = MaterialTheme.typography.labelSmall)
                tool.description?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
            }
        if (initial.tools.isEmpty()) Text("Testez la connexion pour découvrir ses outils.")
    }
}
