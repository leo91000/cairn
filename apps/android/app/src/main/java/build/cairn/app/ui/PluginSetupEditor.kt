@file:OptIn(
    ExperimentalLayoutApi::class,
    androidx.compose.material3.ExperimentalMaterial3Api::class,
)

package build.cairn.app.ui

import androidx.browser.customtabs.CustomTabsIntent
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.net.toUri
import androidx.lifecycle.viewModelScope
import build.cairn.app.data.*
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

/**
 * The setup assistant of one instance, as on the web: the steps the installation resolved (or
 * derived from the manifest), one at a time. Each step shows its checklist, warning and settings,
 * the credential (token or sign-in), the delegation or the agents granted at creation. The
 * package's form handler completes the draft as the owner goes; a new instance can reuse a shared
 * step (such as Gmail's OAuth client) from another instance without the secret reaching the app.
 * Editing opens every step and saves from any of them.
 */
@Composable
internal fun InstanceEditor(
    vm: CairnViewModel,
    state: Workspace,
    pluginId: String,
    entry: PluginEntry,
    manifest: PluginManifest,
    initial: PluginConnection,
    connections: List<PluginConnection>,
    close: () -> Unit,
    openAgents: () -> Unit,
    saved: suspend () -> Unit,
) {
    val creating = initial.id.isBlank()
    val fields = manifest.settings.filter { !it.hidden }
    // Hidden settings are the package's own (e.g. names it derives); editing keeps them.
    val stored = manifest.settings.filter { it.type != "secret" }
    val setup = remember(entry) { entry.resolvedSetup() }
    // A package with nothing to enter still gets one step, for its name.
    val steps = setup.steps.ifEmpty { listOf(SetupStep("details", "settings", "Details")) }
    val mine = connections.filter { it.pluginId == pluginId }
    val sources = sharedSources(steps, manifest.settings, mine, initial.id.takeIf { !creating })
    val sharedKeys = steps.filter { it.shared }.flatMap { it.settings }
    val callbackShown = steps.any { item -> item.checklist.any { it.copy == "callbackUrl" } }
    /** The reused instance's visible shared values; another app starts empty. */
    fun reusedValues(source: String): Map<String, String> {
        val from = mine.find { it.id == source }
        return stored
            .filter { it.key in sharedKeys }
            .associate { it.key to formValue(it, from?.settings?.get(it.key) ?: JsonPrimitive("")) }
    }
    var step by remember(initial.id) { mutableIntStateOf(0) }
    // The saved instance whose shared setup a new one reuses; the first one by default.
    var reuse by
        remember(initial.id) {
            mutableStateOf(if (creating) sources.firstOrNull()?.id.orEmpty() else "")
        }
    var name by remember(initial.id) { mutableStateOf(initial.name) }
    // A name typed by the owner is never replaced by the one the package derives.
    var named by remember(initial.id) { mutableStateOf(!creating) }
    // Deliberately not saveable: never persist a credential in saved instance state.
    var token by remember(initial.id) { mutableStateOf("") }
    var values by
        remember(initial.id) {
            mutableStateOf(
                stored.associate { field ->
                    field.key to formValue(field, initial.settings[field.key])
                } + if (reuse.isNotEmpty()) reusedValues(reuse) else emptyMap()
            )
        }
    // Same rule for secret fields: typed in, sent once, never kept.
    var secrets by remember(initial.id) { mutableStateOf(mapOf<String, String>()) }
    var agentId by remember(initial.id) { mutableStateOf(initial.agentId) }
    var accessMode by remember(initial.id) { mutableStateOf(initial.accessMode) }
    var people by
        remember(initial.id) {
            mutableStateOf(
                initial.userIds.map { id ->
                    PluginLabel(id, initial.people.find { it.id == id }?.name.orEmpty())
                }
            )
        }
    var person by remember(initial.id) { mutableStateOf("") }
    var perHour by remember(initial.id) { mutableStateOf(initial.maxMessagesPerHour.toString()) }
    var perPerson by
        remember(initial.id) { mutableStateOf((initial.maxPerPersonPerHour ?: 5).toString()) }
    var enabled by remember(initial.id) { mutableStateOf(initial.enabled) }
    // Agents granted when the instance is created; later grants are made on agent pages.
    var grants by remember(initial.id) { mutableStateOf(listOf<String>()) }
    // A device sign-in gives the draft its credential once completed; the token stays on the
    // installation. Not saveable either: a flow outlives neither the editor nor the process.
    var signIn by remember(initial.id) { mutableStateOf<PluginDeviceFlow?>(null) }
    var signInError by remember(initial.id) { mutableStateOf<String?>(null) }
    var signingIn by remember(initial.id) { mutableStateOf(false) }
    val signInKind = if (manifest.credential?.oauth != null) "oauth" else "device-flow"
    var oauthCallbackUrl by remember(initial.id) { mutableStateOf("") }
    LaunchedEffect(pluginId) {
        if (manifest.credential?.oauth != null) {
            runCatching { vm.api.get<JsonObject>("/plugins/${segment(pluginId)}/oauth") }
                .onSuccess {
                    oauthCallbackUrl = it["callbackUrl"]?.jsonPrimitive?.content.orEmpty()
                }
                .onFailure { signInError = it.message }
        }
    }
    var completion by remember(initial.id) { mutableStateOf<PluginForm?>(null) }
    var completionInputs by remember(initial.id) { mutableStateOf(mapOf<String, String?>()) }
    // While the package answers, its derived values aren't in the form yet: no saving.
    var completing by remember(initial.id) { mutableStateOf(manifest.instanceForm) }
    // Each change asks the package again; a newer question cancels the older one.
    var asking by remember(initial.id) { mutableStateOf(0 to listOf<String?>(null)) }
    /** What this instance's people could do through an agent, for the owner's warning. */
    fun risks(agent: Agent): List<String> =
        agent.delegationRisks(
            connections,
            everyone = accessMode == "everyone",
            draftGrants = if (creating) grants else emptyList(),
        )
    fun refresh(key: String?) {
        asking = (asking.first + 1) to listOf(key)
    }
    /** The installation copies the reused values, secrets included, without sending them here. */
    fun JsonObjectBuilder.sharing() {
        if (reuse.isNotEmpty()) put("sharedFrom", reuse)
    }
    fun chooseSource(source: String) {
        reuse = source
        values = values + reusedValues(source)
        secrets = secrets - sharedKeys.toSet()
        refresh(null)
    }
    val scope = rememberCoroutineScope()
    /** Forgets the sign-in, on the installation too unless a save consumed it. */
    fun cancelSignIn() {
        val current = signIn ?: return
        signIn = null
        signInError = null
        if (current.state != "expired")
            vm.viewModelScope.launch {
                runCatching {
                    vm.api.request(
                        "DELETE",
                        "/plugins/${segment(current.pluginId)}/$signInKind/${segment(current.id)}",
                    )
                }
            }
    }
    // A sign-in holds the OAuth client and scopes it asked for: choosing another app or other
    // permissions afterwards needs a new sign-in rather than a save the installation refuses.
    val signInBasis =
        manifest.credential?.oauth?.let { oauth ->
            val clientId = oauth["clientIdSetting"]?.jsonPrimitive?.contentOrNull
            val scopeKeys = (oauth["scopeSettings"] as? JsonObject)?.keys.orEmpty()
            val clientSecret = oauth["clientSecretSetting"]?.jsonPrimitive?.contentOrNull
            Triple(
                reuse,
                (listOfNotNull(clientId) + scopeKeys).map { values[it] },
                clientSecret?.let { secrets[it] },
            )
        }
    LaunchedEffect(signInBasis) {
        if (signIn == null) return@LaunchedEffect
        cancelSignIn()
        refresh("credential")
    }
    fun startSignIn() {
        cancelSignIn()
        signingIn = true
        scope.launch {
            try {
                signIn =
                    vm.api.send<PluginDeviceFlow>(
                        "POST",
                        "/plugins/${segment(pluginId)}/$signInKind",
                        buildJsonObject {
                            if (!creating) put("instance", initial.id)
                            put("settings", pluginSettings(stored, values))
                            put("secrets", pluginSecrets(secrets))
                            sharing()
                        },
                    )
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Exception) {
                signInError = e.message ?: "La connexion n’a pas pu démarrer."
            } finally {
                signingIn = false
            }
        }
    }
    // Polls every 2 s until the sign-in settles; a newer or cancelled flow stops this one.
    val polling = signIn?.takeIf { it.pending }
    LaunchedEffect(polling?.id) {
        val flow = polling ?: return@LaunchedEffect
        while (true) {
            kotlinx.coroutines.delay(2000)
            val next =
                try {
                    vm.api.get<PluginDeviceFlow>(
                        "/plugins/${segment(flow.pluginId)}/$signInKind/${segment(flow.id)}"
                    )
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Exception) {
                    if (signIn?.id != flow.id) return@LaunchedEffect
                    // A forgotten sign-in expired on the installation.
                    if (e is ApiException && e.status == 404) {
                        signIn = flow.copy(state = "expired", message = e.message)
                        return@LaunchedEffect
                    }
                    signInError = e.message
                    continue
                }
            if (signIn?.id != flow.id) return@LaunchedEffect
            signInError = null
            signIn = next
            if (!next.pending) {
                // The package's form handler then says who it signed in as.
                if (next.completed && manifest.instanceForm) refresh("credential")
                return@LaunchedEffect
            }
        }
    }
    // Read the draft state directly: saving can consume the sign-in and close the
    // editor before another composition updates a remembered copy.
    DisposableEffect(initial.id) {
        onDispose {
            signIn
                ?.takeIf { it.state != "expired" }
                ?.let { current ->
                    vm.viewModelScope.launch {
                        runCatching {
                            vm.api.request(
                                "DELETE",
                                "/plugins/${segment(current.pluginId)}/$signInKind/${segment(current.id)}",
                            )
                        }
                    }
                }
        }
    }
    if (manifest.instanceForm)
        LaunchedEffect(initial.id, asking) {
            completing = true
            if (asking.second.any { it != null }) kotlinx.coroutines.delay(400)
            for (changed in asking.second) {
                val inputs = fields.associate { field ->
                    field.key to
                        if (field.type == "secret") secrets[field.key] else values[field.key]
                }
                runCatching {
                    val result =
                        vm.api.pluginOperation(
                            "/plugins/${segment(pluginId)}/form",
                            buildJsonObject {
                                if (!creating) put("instance", initial.id)
                                put("settings", pluginSettings(stored, values))
                                put("secrets", pluginSecrets(secrets))
                                sharing()
                                credentialInput(token, signIn)?.let { (key, value) ->
                                    put(key, value)
                                }
                                changed?.let { put("changed", it) }
                            },
                        )
                    wireJson.decodeFromJsonElement<PluginForm>(result)
                }
                    .onSuccess { form ->
                        val derived =
                            form.values.mapValues { (key, value) ->
                                formValue(
                                    stored.find { it.key == key } ?: PluginSetting(key = key),
                                    value,
                                )
                            }
                        values = values + derived
                        completionInputs = inputs + derived
                        // A new instance is named after what the package verified, e.g. the
                        // mailbox.
                        setup.nameFrom
                            ?.let { values[it] }
                            ?.takeIf { it.isNotEmpty() && !named }
                            ?.let { name = it }
                        completion = form
                    }
                    .onFailure { error ->
                        if (error is kotlinx.coroutines.CancellationException) throw error
                        completion =
                            PluginForm(
                                options = completion?.options.orEmpty(),
                                errors =
                                    mapOf(
                                        "form" to (error.message ?: "Le plugin n’a pas répondu.")
                                    ),
                            )
                    }
            }
            completing = false
        }
    // Ordinary fields need not refresh while typing. Errors apply to the input checked by
    // the package; changing it removes a stale error, and save still validates authoritatively.
    val formErrors =
        completion?.errors.orEmpty().filter { (key, _) ->
            val field = fields.find { it.key == key }
            field == null ||
                completionInputs[key] == if (field.type == "secret") secrets[key] else values[key]
        }
    val agent = state.agents.find { it.id == agentId }
    fun filled(field: PluginSetting): Boolean =
        when (field.type) {
            "secret" -> !secrets[field.key].isNullOrBlank() || field.key in initial.secretKeys
            "boolean" -> true
            "multiselect" -> multiselect(values[field.key].orEmpty()).isNotEmpty()
            // A choice is an exact identifier; only text uses blankness.
            "select" -> !values[field.key].isNullOrEmpty()
            else -> !values[field.key].isNullOrBlank()
        }
    /** What a step still needs before the owner can go on, or null when it is complete. */
    fun stepProblem(item: SetupStep): String? =
        when (item.kind) {
            "settings" ->
                if (item.shared && reuse.isNotEmpty()) null
                else {
                    val shown = item.fields(fields)
                    shown.find { it.required && !filled(it) }?.let { "Renseignez « ${it.title} »." }
                        ?: shown.firstNotNullOfOrNull { formErrors[it.key] }
                }
            "credential" ->
                formErrors["credential"]
                    ?: when {
                        !creating || signIn?.completed == true -> null
                        manifest.credential?.oauth != null -> "Connectez-vous pour continuer."
                        token.isNotBlank() -> null
                        else -> "Renseignez « ${manifest.credential?.label ?: "l’identifiant"} »."
                    }
            "people" ->
                when {
                    !manifest.delegates -> null
                    agent == null -> "Choisissez l’agent qui répond."
                    accessMode == "allowlist" && people.isEmpty() ->
                        "Ajoutez au moins une personne."
                    else -> null
                }
            else -> null
        }
    val index = step.coerceIn(0, steps.lastIndex)
    val current = steps[index]
    val last = index == steps.lastIndex
    val sharedReused = current.shared && reuse.isNotEmpty()
    /** Steps before the first incomplete one can be opened; an edit opens any. */
    fun reachable(target: Int) = !creating || steps.take(target).all { stepProblem(it) == null }
    /** Package guidance belongs with the credential and with choices the package fills. */
    fun guided(item: SetupStep) =
        item.kind == "credential" ||
            (manifest.credential == null && item == steps.first()) ||
            item.fields(fields).any { it.dynamic || it.refresh }
    fun holder(agent: Agent): String? =
        if (manifest.onePerAgent) mine.find { agent.id in it.agentIds }?.name else null
    val saving = creating.not() || last
    val ready =
        !state.busy &&
            !completing &&
            if (saving) name.isNotBlank() && steps.all { stepProblem(it) == null }
            else stepProblem(current) == null
    fun save() = vm.perform {
        // Grants to send back (older or unknown installation), as saved right now.
        val keptGrants =
            if (creating) null
            else
                initial.agentIdsToSend(state.agents, manifest) {
                    wireJson
                        .decodeFromString<PluginCatalog>(api.request("GET", "/plugins"))
                        .connections
                        .find { it.id == initial.id }
                        ?.agentIds
                }
        api.pluginOperation(
            "/plugins/${segment(pluginId)}/connections" +
                if (creating) "" else "/${segment(initial.id)}",
            buildJsonObject {
                put("name", name.trim())
                // The save consumes a completed sign-in.
                credentialInput(token, signIn)?.let { (key, value) -> put(key, value) }
                put("settings", pluginSettings(stored, values))
                // Blank secret fields keep what is saved.
                put("secrets", pluginSecrets(secrets))
                if (manifest.delegates) {
                    put("agentId", agentId)
                    put(
                        "projectId",
                        // An agent with one project answers in it; a broader one outside projects.
                        agent?.access?.projects?.singleOrNull()?.let(::JsonPrimitive) ?: JsonNull,
                    )
                    put("accessMode", accessMode)
                    put("userIds", JsonArray(people.map { JsonPrimitive(it.id) }))
                    put(
                        "people",
                        JsonArray(
                            people.map { label ->
                                buildJsonObject {
                                    put("id", label.id)
                                    put("name", label.name)
                                }
                            }
                        ),
                    )
                    put("maxMessagesPerHour", perHour.toIntOrNull() ?: 20)
                    put("maxPerPersonPerHour", perPerson.toIntOrNull() ?: 5)
                }
                put("enabled", enabled)
                sharing()
                // An older (or unknown) installation needs the granted agents back, as they are
                // now. A new instance is granted to the agents chosen on the agents step; creation
                // only grants, it never removes.
                (keptGrants ?: grants.takeIf { creating && it.isNotEmpty() })?.let { ids ->
                    put("agentIds", JsonArray(ids.map(::JsonPrimitive)))
                }
            },
            method = if (creating) "POST" else "PUT",
        )
        token = ""
        // Consumed by the save: nothing is left to cancel.
        signIn = null
        secrets = emptyMap()
        notify("${manifest.noun()} enregistré")
        saved()
    }
    fun next() {
        if (saving) save() else if (stepProblem(current) == null) step = index + 1
    }

    // Each step starts at its top; a failed save brings its message back into view.
    val scroll = remember(index) { ScrollState(0) }
    var revealFeedback by remember { mutableStateOf(false) }
    LaunchedEffect(state.busy, state.error) {
        if (!revealFeedback || state.busy) return@LaunchedEffect
        revealFeedback = false
        if (state.error != null) scroll.animateScrollTo(0)
    }
    Dialog(
        // Back goes to the previous step, then closes the assistant.
        onDismissRequest = {
            if (!state.busy) {
                if (index > 0) step = index - 1 else close()
            }
        },
        properties =
            DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        Scaffold(
            modifier = Modifier.fillMaxSize().imePadding().testTag("plugin-setup"),
            topBar = {
                Column {
                    TopAppBar(
                        title = {
                            Text(
                                if (creating) "Nouveau · ${manifest.noun()}"
                                else "Modifier ${initial.name}",
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        },
                        navigationIcon = {
                            IconButton(onClick = close, enabled = !state.busy) {
                                Icon(Icons.AutoMirrored.Filled.ArrowBack, "Fermer")
                            }
                        },
                    )
                    if (steps.size > 1)
                        StepRail(
                            steps.map { it.shownTitle(manifest) },
                            index,
                            done = { it < index && stepProblem(steps[it]) == null },
                            reachable = { !state.busy && reachable(it) },
                        ) {
                            step = it
                        }
                    Column(Modifier.padding(horizontal = 20.dp)) { ConnectionFeedbackNotice() }
                }
            },
            bottomBar = {
                Surface(color = MaterialTheme.colorScheme.surface, shadowElevation = 6.dp) {
                    Column(
                        Modifier.fillMaxWidth()
                            .navigationBarsPadding()
                            .padding(horizontal = 16.dp, vertical = 10.dp),
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        // What is missing before going on.
                        if (!saving)
                            stepProblem(current)?.let {
                                Text(
                                    it,
                                    Modifier.testTag("plugin-setup-status"),
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        Row(
                            Modifier.fillMaxWidth(),
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            if (index > 0)
                                TextButton(onClick = { step = index - 1 }, enabled = !state.busy) {
                                    Text("Retour")
                                }
                            else
                                TextButton(onClick = close, enabled = !state.busy) {
                                    Text("Annuler")
                                }
                            Spacer(Modifier.weight(1f))
                            if (!creating && !last)
                                OutlinedButton(
                                    onClick = { step = index + 1 },
                                    enabled = !state.busy,
                                ) {
                                    Text("Suivant")
                                }
                            SignalButton(
                                if (saving) "Enregistrer" else "Continuer",
                                enabled = ready,
                                height = 40.dp,
                            ) {
                                if (saving) revealFeedback = true
                                next()
                            }
                        }
                    }
                }
            },
        ) { padding ->
            Column(
                Modifier.padding(padding).fillMaxSize().verticalScroll(scroll).padding(20.dp),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                completion?.errors?.get("form")?.let { FormError(it) }
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    if (steps.size > 1) Eyebrow("Étape ${index + 1} sur ${steps.size}")
                    Text(
                        current.shownTitle(manifest),
                        Modifier.semantics { heading() },
                        style = MaterialTheme.typography.titleLarge,
                    )
                    if (current.description.isNotBlank()) Note(current.description)
                }

                if (current.shared && sources.isNotEmpty() && creating)
                    Column(
                        Modifier.selectableGroup(),
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        sources.forEach { source ->
                            RadioCard(
                                reuse == source.id,
                                "Réutiliser la configuration de ${source.names.joinToString(", ")}",
                                enabled = !state.busy,
                                detail =
                                    source.label
                                        .takeIf { it.isNotEmpty() }
                                        ?.let { shortValue(it, 14) },
                            ) {
                                chooseSource(source.id)
                            }
                        }
                        RadioCard(
                            reuse.isEmpty(),
                            "En configurer une nouvelle",
                            enabled = !state.busy,
                        ) {
                            chooseSource("")
                        }
                    }

                if (!sharedReused) {
                    if (current.checklist.isNotEmpty())
                        key(current.id) { SetupChecklist(current.checklist, oauthCallbackUrl) }
                    if (current.warning.isNotBlank()) SetupWarning(current.warning)
                }

                if (current.kind == "settings" && !sharedReused) {
                    val shown = current.fields(fields)
                    val switches = shown.filter { it.type == "boolean" }
                    if (current.presets.isNotEmpty())
                        SetupPresets(current, fields, values, enabled = !state.busy) {
                            val changed =
                                fields
                                    .filter { field ->
                                        field.refresh &&
                                            field.key in it &&
                                            values[field.key] != it[field.key]
                                    }
                                    .map { field -> field.key }
                            values = values + it
                            if (changed.isNotEmpty()) asking = (asking.first + 1) to changed
                        }
                    PluginFields(
                        if (current.presets.isNotEmpty()) shown.filter { it.type != "boolean" }
                        else shown,
                        values,
                        { key, value -> values = values + (key to value) },
                        secrets,
                        { key, value -> secrets = secrets + (key to value) },
                        saved = initial.secretKeys,
                        enabled = !state.busy,
                        options = completion?.options.orEmpty(),
                        errors = formErrors,
                        refresh = ::refresh,
                    )
                    if (current.presets.isNotEmpty() && switches.isNotEmpty())
                        FineTune(current.matchingPreset(fields, values).isEmpty(), current.id) {
                            PluginFields(
                                switches,
                                values,
                                { key, value -> values = values + (key to value) },
                                secrets,
                                { key, value -> secrets = secrets + (key to value) },
                                enabled = !state.busy,
                                errors = formErrors,
                                refresh = ::refresh,
                            )
                        }
                }

                if (current.kind == "credential")
                    manifest.credential?.let { credential ->
                        val problem = completion?.errors?.get("credential")
                        val signedIn = signIn?.completed == true
                        if (credential.oauth == null) {
                            OutlinedTextField(
                                token,
                                {
                                    token = it
                                    // A pasted token replaces a sign-in.
                                    if (signIn != null) cancelSignIn()
                                    if (manifest.instanceForm) refresh("credential")
                                },
                                Modifier.fillMaxWidth(),
                                label = {
                                    Text(
                                        if (creating) credential.label
                                        else "Nouveau ${credential.label.lowercase()} (facultatif)"
                                    )
                                },
                                singleLine = true,
                                visualTransformation = PasswordVisualTransformation(),
                                keyboardOptions = InputKeyboards.Password,
                                enabled = !state.busy,
                                isError = problem != null,
                                placeholder =
                                    when {
                                        signedIn -> ({
                                                Text("Connecté · ou collez un jeton à la place")
                                            })
                                        creating -> null
                                        else -> ({
                                                Text("Enregistré · laissez vide pour le garder")
                                            })
                                    },
                                supportingText = {
                                    Text(
                                        problem
                                            ?: "Chiffré. Cairn l’ajoute aux requêtes de ce plugin ; le plugin ne le voit jamais."
                                    )
                                },
                            )
                        } else if (problem != null) {
                            FormError(problem)
                        }
                        if (
                            credential.oauth != null &&
                                oauthCallbackUrl.isNotBlank() &&
                                !callbackShown
                        ) {
                            Text("Adresse de retour à enregistrer dans l’application OAuth")
                            SelectionContainer { Text(oauthCallbackUrl) }
                            CopyButton("Copier l’adresse de retour", oauthCallbackUrl)
                        }
                        if (credential.deviceFlow != null || credential.oauth != null)
                            DeviceSignIn(
                                manifest,
                                signIn,
                                signInError,
                                label =
                                    if (creating) current.shownTitle(manifest)
                                    else "Se reconnecter",
                                last = last,
                                enabled = !state.busy && !signingIn,
                                start = ::startSignIn,
                                cancel = ::cancelSignIn,
                            )
                    }

                val view = completion?.view ?: JsonArray(emptyList())
                if (guided(current) && (view.isNotEmpty() || completing))
                    Column(
                        Modifier.testTag("plugin-form-view"),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        if (completing && view.isEmpty()) Note("Vérification…")
                        PluginViewNodes(view, state.busy, press = {}, act = { _, _, _ -> })
                    }

                if (current.kind == "people" && manifest.delegates) {
                    Choice(
                        "Qui peut lui parler",
                        accessMode,
                        listOf(
                            "requests" to "Valider les personnes à leur demande",
                            "allowlist" to "Seulement les personnes que j’ajoute",
                            "everyone" to "Tout le monde",
                        ),
                    ) {
                        accessMode = it
                    }
                    if (accessMode == "everyone")
                        Note(
                            "Toute personne qui peut écrire dans le salon, avec une limite horaire par personne."
                        )
                    Text("Personnes autorisées", style = MaterialTheme.typography.labelLarge)
                    people.forEach { label ->
                        val shown = label.name.ifBlank { "Personne" }
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            AgentAvatar(label.name.ifBlank { label.id }, label.id, 28.dp)
                            Spacer(Modifier.width(10.dp))
                            Column(Modifier.weight(1f)) {
                                Text(
                                    shown,
                                    style = MaterialTheme.typography.bodyMedium,
                                    maxLines = 1,
                                )
                                Text(
                                    label.id,
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            ActionIcon("Retirer $shown", CairnIcons.Close, enabled = !state.busy) {
                                people = people.filter { it.id != label.id }
                            }
                        }
                    }
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Box(Modifier.weight(1f)) {
                            Field(
                                "Identifiant d’une personne",
                                person,
                                { person = it.trim() },
                                enabled = !state.busy,
                            )
                        }
                        Spacer(Modifier.width(8.dp))
                        TextButton(
                            onClick = {
                                if (people.none { it.id == person }) {
                                    val known =
                                        connections
                                            .flatMap { it.requests }
                                            .find { it.userId == person }
                                            ?.name
                                            .orEmpty()
                                    people = people + PluginLabel(person, known)
                                }
                                person = ""
                            },
                            enabled = person.isNotBlank() && !state.busy,
                        ) {
                            Text("Ajouter")
                        }
                    }
                    ChoiceList(
                        "Répond en tant que",
                        agentId,
                        state.agents
                            .sortedBy { risks(it).isNotEmpty() }
                            .map { agent ->
                                PluginOption(
                                    agent.id,
                                    agent.name,
                                    if (risks(agent).isEmpty()) "" else "Accès large",
                                )
                            },
                        enabled = !state.busy,
                    ) {
                        agentId = it
                    }
                    Note(
                        "Les personnes parlent à cet agent avec tous ses accès. Créez un agent dédié pour limiter ce qu’elles peuvent faire."
                    )
                    Field(
                        "Messages par heure",
                        perHour,
                        { perHour = it.filter(Char::isDigit).take(3) },
                        enabled = !state.busy,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                    )
                    if (accessMode == "everyone")
                        Field(
                            "Messages par personne et par heure",
                            perPerson,
                            { perPerson = it.filter(Char::isDigit).take(2) },
                            enabled = !state.busy,
                            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                        )
                }

                if (current.kind == "agents")
                    manifest.agentAccess?.let { access ->
                        if (access.description.isNotBlank()) Note(access.description)
                        if (creating) {
                            Text(
                                "Agents qui peuvent l’utiliser",
                                style = MaterialTheme.typography.labelLarge,
                            )
                            state.agents.forEach { agent ->
                                AgentGrant(
                                    agent.name,
                                    agent.id in grants,
                                    holder(agent),
                                    enabled = !state.busy,
                                ) { checked ->
                                    grants = if (checked) grants + agent.id else grants - agent.id
                                }
                            }
                            Note(
                                "Vous pouvez passer cette étape : aucun agent ne peut l’utiliser tant que vous ne lui en donnez pas l’accès ici ou sur sa fiche."
                            )
                        } else {
                            val names =
                                initial.agentIds.map { id ->
                                    state.agents.find { it.id == id }?.name ?: "Agent supprimé"
                                }
                            Text(
                                if (names.isEmpty())
                                    "Aucun agent ne peut l’utiliser pour l’instant."
                                else "Agents autorisés : ${names.joinToString(", ")}",
                                style = MaterialTheme.typography.bodyMedium,
                            )
                            Note(
                                "Les accès se règlent sur la fiche de chaque agent (Secrets & plugins)."
                            )
                            TextButton(onClick = openAgents) {
                                Text("Gérer sur les fiches agents ›")
                            }
                        }
                    }

                if (manifest.delegates && current.kind in listOf("people", "agents")) {
                    val chosenRisks = agent?.let(::risks).orEmpty()
                    if (agent != null && chosenRisks.isNotEmpty()) {
                        val everyone = accessMode == "everyone"
                        SetupWarning(
                            title =
                                if (everyone)
                                    "Tout le monde dans le salon peut utiliser tout ce que ${agent.name} sait faire"
                                else
                                    "Les personnes autorisées peuvent utiliser tout ce que ${agent.name} sait faire",
                            text =
                                "Via le bot, il est possible de ${listed(chosenRisks)}. " +
                                    if (everyone)
                                        "Pour un salon public, préférez un agent en lecture seule avec un seul projet."
                                    else
                                        "N’autorisez que des personnes de confiance, ou choisissez un agent plus limité.",
                        )
                    }
                }

                if (saving) {
                    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    Field(
                        "Nom",
                        name,
                        {
                            name = it
                            named = true
                        },
                        enabled = !state.busy,
                    )
                    Toggle("Active", enabled) { if (!state.busy) enabled = it }
                }
            }
        }
    }
}

/**
 * The assistant's steps in a horizontal list under the title: done, current or still to come. Steps
 * after the first incomplete one can't be opened yet.
 */
@Composable
private fun StepRail(
    titles: List<String>,
    current: Int,
    done: (Int) -> Boolean,
    reachable: (Int) -> Boolean,
    open: (Int) -> Unit,
) {
    val scroll = rememberScrollState()
    Row(
        Modifier.fillMaxWidth()
            .horizontalScroll(scroll)
            .padding(horizontal = 16.dp, vertical = 4.dp)
            .testTag("plugin-setup-steps"),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        titles.forEachIndexed { index, title ->
            val here = index == current
            val finished = done(index)
            val canOpen = reachable(index)
            Row(
                Modifier.clip(RoundedCornerShape(12.dp))
                    .background(
                        if (here) MaterialTheme.colorScheme.primaryContainer
                        else MaterialTheme.colorScheme.surfaceContainer
                    )
                    .clickable(enabled = canOpen, role = Role.Tab) { open(index) }
                    .semantics {
                        contentDescription =
                            "Étape ${index + 1} sur ${titles.size} : $title" +
                                if (finished) " · terminée" else ""
                        selected = here
                    }
                    .alpha(if (canOpen || here) 1f else 0.5f)
                    .padding(horizontal = 10.dp, vertical = 7.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                // The row speaks for itself: its texts are not separate nodes.
                Row(
                    Modifier.clearAndSetSemantics {},
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    StepNumber(index + 1, finished, here)
                    Spacer(Modifier.width(7.dp))
                    Text(
                        title,
                        style = MaterialTheme.typography.labelLarge,
                        fontWeight = if (here) FontWeight.SemiBold else FontWeight.Normal,
                        maxLines = 1,
                    )
                }
            }
        }
    }
}

@Composable
private fun StepNumber(number: Int, done: Boolean, current: Boolean) {
    val color =
        when {
            done -> signal.success
            current -> MaterialTheme.colorScheme.primary
            else -> MaterialTheme.colorScheme.outline
        }
    Box(
        Modifier.size(22.dp)
            .clip(CircleShape)
            .background(if (done) signal.success else MaterialTheme.colorScheme.surface)
            .border(1.dp, color, CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        if (done)
            Icon(
                CairnIcons.Check,
                null,
                Modifier.size(12.dp),
                tint = MaterialTheme.colorScheme.surface,
            )
        else
            Text(
                "$number",
                style = MaterialTheme.typography.labelSmall,
                fontWeight = FontWeight.Bold,
                color = color,
            )
    }
}

/**
 * What the owner does on the service's site before Cairn can connect, ticked off as they go. Ticks
 * only help the owner keep their place; nothing is saved. Opening a task's page ticks it.
 */
@Composable
private fun SetupChecklist(tasks: List<SetupTask>, callbackUrl: String) {
    val done = remember(tasks) { mutableStateListOf(*Array(tasks.size) { false }) }
    val context = LocalContext.current
    Column(
        Modifier.testTag("plugin-setup-checklist"),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Eyebrow("À faire d’abord")
        tasks.forEachIndexed { index, task ->
            val ticked = done[index]
            Surface(
                shape = RoundedCornerShape(14.dp),
                color =
                    if (ticked) MaterialTheme.colorScheme.surfaceContainer
                    else MaterialTheme.colorScheme.surface,
                border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Row(
                    Modifier.padding(start = 2.dp, end = 12.dp, top = 2.dp, bottom = 10.dp),
                    verticalAlignment = Alignment.Top,
                ) {
                    Checkbox(
                        ticked,
                        { done[index] = it },
                        Modifier.semantics { contentDescription = "${task.title} : fait" },
                    )
                    Column(
                        Modifier.weight(1f).padding(top = 12.dp),
                        verticalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        Text(
                            "${index + 1}. ${task.title}",
                            style = MaterialTheme.typography.titleSmall,
                            textDecoration = if (ticked) TextDecoration.LineThrough else null,
                            color =
                                if (ticked) MaterialTheme.colorScheme.onSurfaceVariant
                                else MaterialTheme.colorScheme.onSurface,
                        )
                        if (task.text.isNotBlank()) Note(task.text)
                        if (task.copy == "callbackUrl" && callbackUrl.isNotBlank()) {
                            SelectionContainer {
                                Text(
                                    callbackUrl,
                                    Modifier.semantics { contentDescription = "Adresse de retour" },
                                    fontFamily = FontFamily.Monospace,
                                    style = MaterialTheme.typography.labelMedium,
                                )
                            }
                            CopyButton("Copier l’adresse de retour", callbackUrl)
                        }
                        task.url
                            ?.toUri()
                            ?.takeIf { it.scheme == "https" && it.host != null }
                            ?.let { uri ->
                                OutlinedButton(
                                    onClick = {
                                        done[index] = true
                                        runCatching {
                                            CustomTabsIntent.Builder()
                                                .build()
                                                .launchUrl(context, uri)
                                        }
                                    }
                                ) {
                                    Text(task.link?.ifBlank { null } ?: "Ouvrir")
                                    Spacer(Modifier.width(6.dp))
                                    Icon(CairnIcons.External, null, Modifier.size(14.dp))
                                }
                            }
                    }
                }
            }
        }
    }
}

/** A consequence to know before going on, e.g. a provider's token expiry. */
@Composable
private fun SetupWarning(text: String, title: String? = null) {
    Row(
        Modifier.fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .background(signal.warningSoft)
            .padding(12.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Icon(
            CairnIcons.Alert,
            null,
            Modifier.padding(top = 1.dp).size(16.dp),
            tint = signal.warning,
        )
        Spacer(Modifier.width(10.dp))
        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
            if (title != null)
                Text(
                    title,
                    style = MaterialTheme.typography.bodySmall,
                    fontWeight = FontWeight.SemiBold,
                    color = signal.warning,
                )
            Text(text, style = MaterialTheme.typography.bodySmall, color = signal.warning)
        }
    }
}

/**
 * A step's switches as a few named choices, e.g. "Read only". Choosing one sets every switch of the
 * step; fine-tuned switches that match none leave none selected.
 */
@Composable
private fun SetupPresets(
    step: SetupStep,
    fields: List<PluginSetting>,
    values: Map<String, String>,
    enabled: Boolean,
    choose: (Map<String, String>) -> Unit,
) {
    val selected = step.matchingPreset(fields, values)
    Column(Modifier.selectableGroup(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        step.presets.forEach { preset ->
            RadioCard(
                selected == preset.id,
                preset.title,
                enabled = enabled,
                description = preset.description.takeIf { it.isNotBlank() },
                risk = preset.risk,
            ) {
                choose(step.presetValues(fields, preset))
            }
        }
        if (selected.isEmpty()) Note("Permissions personnalisées, réglées ci-dessous.")
    }
}

/** The step's switches one by one, open while they match no preset. */
@Composable
private fun FineTune(custom: Boolean, step: String, content: @Composable ColumnScope.() -> Unit) {
    var open by remember(step) { mutableStateOf(custom) }
    LaunchedEffect(step, custom) { open = custom }
    Surface(
        shape = RoundedCornerShape(14.dp),
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        color = MaterialTheme.colorScheme.surface,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column {
            Row(
                Modifier.fillMaxWidth()
                    .toggleable(open, role = Role.Button) { open = it }
                    .padding(horizontal = 14.dp, vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text("Affiner", Modifier.weight(1f), style = MaterialTheme.typography.titleSmall)
                Icon(if (open) CairnIcons.Up else CairnIcons.Down, null, Modifier.size(16.dp))
            }
            if (open)
                Column(
                    Modifier.padding(start = 14.dp, end = 14.dp, bottom = 10.dp),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                    content = content,
                )
        }
    }
}

/** One choice of a radio group drawn as a card, with an optional risk and explanation. */
@Composable
private fun RadioCard(
    chosen: Boolean,
    title: String,
    enabled: Boolean = true,
    description: String? = null,
    /** A short value set in monospace, e.g. the reused OAuth client ID. */
    detail: String? = null,
    risk: String? = null,
    choose: () -> Unit,
) {
    Surface(
        selected = chosen,
        onClick = choose,
        enabled = enabled,
        shape = RoundedCornerShape(12.dp),
        color =
            if (chosen) MaterialTheme.colorScheme.primaryContainer
            else MaterialTheme.colorScheme.surface,
        border =
            BorderStroke(
                1.dp,
                if (chosen) MaterialTheme.colorScheme.primary
                else MaterialTheme.colorScheme.outlineVariant,
            ),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            Modifier.padding(horizontal = 12.dp, vertical = 10.dp),
            verticalAlignment = Alignment.Top,
        ) {
            RadioButton(chosen, null, enabled = enabled)
            Spacer(Modifier.width(8.dp))
            Column(
                Modifier.weight(1f).padding(top = 2.dp),
                verticalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                FlowRow(
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Text(title, style = MaterialTheme.typography.titleSmall)
                    RiskPill(risk)
                }
                if (detail != null)
                    Text(
                        detail,
                        fontFamily = FontFamily.Monospace,
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                if (description != null) Note(description)
            }
        }
    }
}

/** One agent a new instance can be granted to, unless it already uses another one. */
@Composable
private fun AgentGrant(
    name: String,
    checked: Boolean,
    holder: String?,
    enabled: Boolean,
    change: (Boolean) -> Unit,
) {
    val allowed = enabled && holder == null
    Surface(
        shape = RoundedCornerShape(12.dp),
        color =
            if (checked) MaterialTheme.colorScheme.primaryContainer
            else MaterialTheme.colorScheme.surface,
        border =
            BorderStroke(
                1.dp,
                if (checked) MaterialTheme.colorScheme.primary
                else MaterialTheme.colorScheme.outlineVariant,
            ),
        modifier = Modifier.fillMaxWidth().alpha(if (holder != null) 0.6f else 1f),
    ) {
        Row(
            Modifier.toggleable(
                    checked,
                    enabled = allowed,
                    role = Role.Checkbox,
                    onValueChange = change,
                )
                .padding(horizontal = 12.dp, vertical = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Checkbox(checked, null, enabled = allowed)
            Spacer(Modifier.width(8.dp))
            Text(
                name,
                Modifier.weight(1f),
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (holder != null)
                Pill(
                    "Utilise $holder",
                    MaterialTheme.colorScheme.surfaceVariant,
                    MaterialTheme.colorScheme.onSurfaceVariant,
                )
        }
    }
}

/** How much a switch or a preset lets agents do: low reads as safe, high as a warning. */
@Composable
internal fun RiskPill(risk: String?) {
    val label = riskLabel(risk) ?: return
    when (risk) {
        "low" -> Pill(label, signal.success.copy(alpha = 0.14f), signal.success)
        "medium" -> Pill(label, signal.warningSoft, signal.warning)
        else ->
            Pill(
                label,
                MaterialTheme.colorScheme.error.copy(alpha = 0.12f),
                MaterialTheme.colorScheme.error,
            )
    }
}

/** What connecting takes, in the catalogue and on the first-setup card. */
@Composable
internal fun EffortPill(effort: String?) {
    val label = effortLabel(effort) ?: return
    if (effort == "app") Pill(label, signal.warningSoft, signal.warning)
    else Pill(label, MaterialTheme.colorScheme.primaryContainer, MaterialTheme.colorScheme.primary)
}

/**
 * A plugin without instances yet: what connecting takes, its steps in order (shared ones are set up
 * once), and the way into the assistant.
 */
@Composable
internal fun FirstSetup(manifest: PluginManifest, setup: PluginSetup, start: () -> Unit) {
    SignalCard(Modifier.fillMaxWidth().testTag("plugin-first-setup")) {
        Row(verticalAlignment = Alignment.Top) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(
                    if (manifest.instances != null) "Configurer · ${manifest.noun()}"
                    else "Configurer une instance",
                    Modifier.semantics { heading() },
                    style = MaterialTheme.typography.titleMedium,
                )
                Note(
                    setup.summary.ifBlank {
                        "${manifest.noun(2)} : aucun pour l’instant. Ajoutez-en un pour commencer à utiliser ce plugin."
                    }
                )
            }
            Spacer(Modifier.width(8.dp))
            EffortPill(setup.effort)
        }
        if (setup.steps.isNotEmpty()) {
            Column(Modifier.padding(top = 10.dp)) {
                setup.steps.forEachIndexed { index, item ->
                    if (index > 0)
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    val title = item.shownTitle(manifest)
                    // One node per step, read as a whole.
                    Row(
                        Modifier.fillMaxWidth().padding(vertical = 8.dp).clearAndSetSemantics {
                            contentDescription =
                                "${index + 1}. $title" +
                                    (if (item.shared) " · une fois" else "") +
                                    item.description
                                        .takeIf { it.isNotBlank() }
                                        ?.let { " · $it" }
                                        .orEmpty()
                        },
                        verticalAlignment = Alignment.Top,
                    ) {
                        StepNumber(index + 1, done = false, current = false)
                        Spacer(Modifier.width(10.dp))
                        Column(
                            Modifier.weight(1f),
                            verticalArrangement = Arrangement.spacedBy(2.dp),
                        ) {
                            FlowRow(
                                horizontalArrangement = Arrangement.spacedBy(6.dp),
                                verticalArrangement = Arrangement.spacedBy(4.dp),
                            ) {
                                Text(title, style = MaterialTheme.typography.titleSmall)
                                if (item.shared)
                                    Pill(
                                        "Une fois",
                                        MaterialTheme.colorScheme.surfaceVariant,
                                        MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                            }
                            if (item.description.isNotBlank()) Note(item.description)
                        }
                    }
                }
            }
        }
        SignalButton(
            "Commencer la configuration",
            Modifier.padding(top = 12.dp),
            icon = CairnIcons.Right,
            height = 40.dp,
            onClick = start,
        )
    }
}
