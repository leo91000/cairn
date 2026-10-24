package build.cairn.app.data

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import build.cairn.app.BuildConfig
import java.io.IOException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.add
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray

fun body(vararg pairs: Pair<String, String>) = buildJsonObject {
    pairs.forEach { put(it.first, it.second) }
}

fun segment(value: String): String = java.net.URLEncoder.encode(value, "UTF-8").replace("+", "%20")

data class Workspace(
    val ready: Boolean = false,
    val origin: String = "",
    val session: Session = Session(),
    val installation: Installation? = null,
    val emailForCode: String? = null,
    val busy: Boolean = false,
    val signingOut: Boolean = false,
    val signingIn: Boolean = false,
    val restoringSession: Boolean = false,
    val mcps: List<Mcp> = emptyList(),
    val modelCatalogs: Map<String, ModelCatalog> = emptyMap(),
    val error: String? = null,
    val notice: String? = null,
    /** The workspace could not be reached; cleared on its own once a request succeeds again. */
    val connectionLost: Boolean = false,
    val agents: List<Agent> = emptyList(),
    val projects: List<Project> = emptyList(),
    val tasks: List<Task> = emptyList(),
    val skills: List<Skill> = emptyList(),
    val overview: Overview = Overview(),
    val transportRoute: String = "relay",
) {
    fun modelCatalog(provider: String): ModelCatalog = modelCatalogs[provider] ?: ModelCatalog()

    val isOwner: Boolean
        get() = installation?.role == InstallationRole.Owner
}

class CairnViewModel
@JvmOverloads
constructor(
    application: Application,
    private val vault: SessionVault = KeystoreSessionVault(application),
    private val beaconOrigin: String = BuildConfig.BEACON_SERVICE_ORIGIN,
) : AndroidViewModel(application) {
    val historyCache = HistoryCache.encrypted(application)
    private val retentionPreferences = application.getSharedPreferences("conversation-cache", 0)

    suspend fun acceptCacheRevision(revision: String?) {
        if (revision != null && retentionPreferences.getString(api.cacheScope, null) != revision) {
            historyCache.clear()
            retentionPreferences.edit().putString(api.cacheScope, revision).apply()
        }
    }

    val files = Files(application)
    val chatDrafts = mutableMapOf<String, ChatDraft>()

    private fun clearDrafts() {
        chatDrafts.values.forEach { files.discard(it.attachments) }
        chatDrafts.clear()
    }

    val notifications = NotificationPreferences(application)

    override fun onCleared() {
        connection?.closeStreams()
        accountConnection?.closeStreams()
    }

    private val preferences = Preferences(application)
    val theme = preferences.theme
    val telemetryEnabled = preferences.telemetryEnabled

    fun setTelemetryEnabled(enabled: Boolean) {
        viewModelScope.launch {
            try {
                preferences.setTelemetryEnabled(enabled)
            } catch (e: Exception) {
                report(e)
            }
        }
    }

    internal suspend fun uploadPerformance(report: String) {
        val current = state.value
        val account = accountConnection ?: return
        if (!current.session.authenticated || current.signingOut) return
        account.request(
            "POST",
            "/account/telemetry/performance",
            wireJson.parseToJsonElement(report),
        )
    }

    fun setTheme(value: String) {
        viewModelScope.launch {
            try {
                preferences.setTheme(value)
            } catch (e: Exception) {
                report(e)
            }
        }
    }

    private val mutable = MutableStateFlow(Workspace())
    val state = mutable.asStateFlow()
    private var recovery: kotlinx.coroutines.Job? = null
    private var connection: CairnApi? = null
        set(value) {
            if (field !== value) {
                recovery?.cancel()
                recovery = null
                mutable.update { it.copy(connectionLost = false) }
            }
            field = value
        }

    private var routeCollection: kotlinx.coroutines.Job? = null
    private var accountConnection: CairnApi? = null
    private var emailChallenge: String? = null
    val api: CairnApi
        get() = checkNotNull(connection) { "Choisissez une installation." }

    init {
        perform {
            val origin = beaconOrigin
            if (origin.isNotBlank()) connect(origin)
            else notify("Beacon n’est pas configuré dans cette version de l’application.")
        }
    }

    fun clearNotice() {
        mutable.update { it.copy(notice = null) }
    }

    fun notify(message: String) {
        mutable.update { it.copy(notice = message) }
    }

    fun clearMessage() {
        mutable.update { it.copy(error = null, notice = null) }
    }

    fun report(error: Throwable) {
        if (error is CancellationException) throw error
        if (error is ApiException && error.status == 401 && state.value.session.authenticated) {
            val expiredConnection = connection
            mutable.update {
                Workspace(
                    ready = true,
                    origin = it.origin,
                    error = "Session expirée. Reconnectez-vous.",
                )
            }
            viewModelScope.launch(start = kotlinx.coroutines.CoroutineStart.UNDISPATCHED) {
                transitionScope(forgetSession = true, scheduleNotifications = false)
                if (connection === expiredConnection) connection = null
            }
        } else if (isConnectionFailure(error) && state.value.installation != null) {
            connectionLost()
        } else {
            mutable.update { it.copy(error = error.message ?: "Connexion impossible. Réessayez.") }
            val source = connection
            if (error is ApiException && error.status in setOf(403, 404) && source != null) {
                viewModelScope.launch {
                    state.first { !it.busy }
                    if (connection !== source || !state.value.session.authenticated) return@launch
                    val previous = state.value.installation
                    try {
                        refreshInstallations()
                        val current = state.value.installation
                        if (current?.id != previous?.id || current?.role != previous?.role) {
                            mutable.update {
                                it.copy(
                                    error = null,
                                    notice = "Votre accès à cette installation a changé.",
                                )
                            }
                        }
                    } catch (refreshError: Exception) {
                        if (refreshError is CancellationException) throw refreshError
                        if (refreshError is ApiException && refreshError.status == 401)
                            report(refreshError)
                    }
                }
            }
        }
    }

    /**
     * Clears transient installation state on every transition. Storage survives only an unchanged
     * notification scope and CSRF; a null scope leaves the saved notification scope alone. Callers
     * keep the old API available until suspending cleanup finishes, then publish their destination
     * screen and replace or remove the connection without suspension.
     */
    private suspend fun transitionScope(
        notificationScope: String? = null,
        csrf: String? = null,
        clearStorage: Boolean = true,
        forgetSession: Boolean = false,
        scheduleNotifications: Boolean? = null,
    ) {
        connection?.closeStreams()
        routeCollection?.cancel()
        routeCollection = null
        clearReads()
        clearDrafts()

        if (forgetSession) {
            connection?.clearSession()
            accountConnection?.clearSession()
        }

        if (scheduleNotifications != null) schedule(getApplication(), scheduleNotifications)

        val sessionChanged = csrf != null && connection != null && connection?.csrf != csrf
        val scopeChanged = notificationScope?.let { notifications.selectScope(it) } ?: false
        if (clearStorage || scopeChanged || sessionChanged) {
            historyCache.clear()
            files.clear()
        }
    }

    /**
     * Fil items read on this device, by the end of the agent's work that was read, until the
     * server's lists catch up. Keeping the finish time lets a later turn show as unread again.
     */
    val localReads = androidx.compose.runtime.mutableStateMapOf<String, Long>()
    private var readsGeneration = 0

    /**
     * Mission outcomes set aside on this device, by run, until the mission poll reflects them. A
     * null value brings one back.
     */
    val localDismissals = androidx.compose.runtime.mutableStateMapOf<String, OutcomeDismissal?>()
    private val dismissalRequests = mutableMapOf<String, Any>()
    private val savedDismissals = mutableMapOf<String, Any>()

    private fun clearReads() {
        readsGeneration++
        localReads.clear()
        localDismissals.clear()
        dismissalRequests.clear()
        savedDismissals.clear()
    }

    /** A poll started after a successful write supersedes its optimistic value. */
    suspend fun missionActivity(): List<Run> {
        val target = api
        val generation = readsGeneration
        val saved = savedDismissals.toMap()
        val activity = target.get<List<Run>>("/tasks/activity")
        if (generation == readsGeneration && connection === target) {
            for ((runId, request) in saved) {
                if (dismissalRequests[runId] === request) {
                    localDismissals.remove(runId)
                    dismissalRequests.remove(runId)
                    savedDismissals.remove(runId)
                }
            }
        }
        return activity
    }

    /**
     * Sets a blocked mission's outcome aside for this account, on every device: dismissed, or
     * snoozed [until] a time. It asks again when the mission finishes another run.
     */
    fun setAside(runId: String, taskId: String, finishedAt: Long, until: Long? = null) =
        dismissal(
            runId,
            OutcomeDismissal(until),
            buildJsonObject {
                put("key", "task:$taskId")
                put("finishedAt", finishedAt)
                if (until != null) put("until", until)
            },
            if (until == null) "Écartée jusqu’au prochain résultat de la mission"
            else "Rappel demain à 9 h",
        )

    /** Brings a dismissed or snoozed outcome back to Pour vous. */
    fun restoreOutcome(runId: String, taskId: String) =
        dismissal(
            runId,
            null,
            buildJsonObject {
                put("key", "task:$taskId")
                put("restore", true)
            },
            "De retour dans Pour vous",
        )

    private fun dismissal(
        runId: String,
        value: OutcomeDismissal?,
        item: JsonObject,
        message: String,
    ) {
        val target = connection ?: return
        if (!state.value.session.authenticated || state.value.signingOut) return
        val generation = readsGeneration
        val request = Any()
        dismissalRequests[runId] = request
        savedDismissals.remove(runId)
        localDismissals[runId] = value
        viewModelScope.launch {
            try {
                target.request(
                    "POST",
                    "/dismissals",
                    buildJsonObject { putJsonArray("items") { add(item) } },
                )
                if (generation != readsGeneration || connection !== target) return@launch
                if (dismissalRequests[runId] === request) savedDismissals[runId] = request
                notify(message)
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                if (generation != readsGeneration || connection !== target) return@launch
                if (dismissalRequests[runId] !== request) return@launch
                localDismissals.remove(runId)
                dismissalRequests.remove(runId)
                savedDismissals.remove(runId)
                report(e)
            }
        }
    }

    /** Marks finished work (`chat:<id>` or `task:<id>` with its finish time) as read everywhere. */
    fun markRead(items: List<Pair<String, Long>>) {
        val target = connection ?: return
        if (!state.value.session.authenticated || state.value.signingOut) return
        val unread = items.filter { (key, at) -> (localReads[key] ?: Long.MIN_VALUE) < at }
        if (unread.isEmpty()) return
        val generation = readsGeneration
        viewModelScope.launch {
            for (batch in unread.chunked(200)) {
                if (generation != readsGeneration || connection !== target) return@launch
                val previous = batch.map { (key, _) -> key to localReads[key] }
                batch.forEach { (key, at) -> localReads[key] = maxOf(localReads[key] ?: 0, at) }
                try {
                    target.request(
                        "POST",
                        "/reads",
                        buildJsonObject {
                            putJsonArray("items") {
                                batch.forEach { (key, at) ->
                                    add(
                                        buildJsonObject {
                                            put("key", key)
                                            put("finishedAt", at)
                                        }
                                    )
                                }
                            }
                        },
                    )
                } catch (e: Exception) {
                    if (e is CancellationException) throw e
                    if (generation != readsGeneration || connection !== target) return@launch
                    previous.forEachIndexed { index, (key, at) ->
                        if (localReads[key] == batch[index].second) {
                            if (at == null) localReads.remove(key) else localReads[key] = at
                        }
                    }
                    report(e)
                    return@launch
                }
            }
        }
    }

    /**
     * Shows the connection as lost instead of an error, and probes the installation with growing
     * delays. The first answer clears the notice and reloads the workspace.
     */
    private fun connectionLost() {
        val target = connection ?: return
        mutable.update { it.copy(connectionLost = true) }
        if (recovery?.isActive == true) return
        recovery = viewModelScope.launch {
            var wait = 1_000L
            var failure: Exception? = null
            while (true) {
                delay(wait)
                if (connection !== target || !state.value.session.authenticated) return@launch
                try {
                    target.request("GET", "/overview")
                    break
                } catch (e: Exception) {
                    if (e is CancellationException) throw e
                    if (!isConnectionFailure(e)) {
                        failure = e
                        break
                    }
                }
                wait = (wait * 2).coerceAtMost(10_000L)
            }

            if (connection !== target || !state.value.session.authenticated) return@launch
            mutable.update { it.copy(connectionLost = false) }
            if (failure != null) {
                recovery = null
                report(failure)
                return@launch
            }

            // A successful probe must still reload after any user action finishes.
            state.first { !it.busy }
            if (connection !== target || !state.value.session.authenticated) return@launch
            recovery = null
            perform(userAction = false) {
                refresh()
                if (connection === target) mutable.update { it.copy(connectionLost = false) }
            }
        }
    }

    fun perform(block: suspend CairnViewModel.() -> Unit) =
        perform(userAction = true, block = block)

    private fun perform(userAction: Boolean, block: suspend CairnViewModel.() -> Unit) {
        if (mutable.value.busy) return
        mutable.update { it.copy(busy = true, error = if (userAction) null else it.error) }
        viewModelScope.launch {
            try {
                block()
            } catch (e: Exception) {
                report(e)
                // The connection notice is quiet; still tell the user this action did not happen.
                if (userAction && state.value.connectionLost && isConnectionFailure(e))
                    notify("Connexion impossible. Réessayez.")
            } finally {
                mutable.update { it.copy(busy = false, ready = true) }
            }
        }
    }

    // The shipped app supplies only BuildConfig.BEACON_SERVICE_ORIGIN. Tests inject an HTTP
    // fixture.
    internal suspend fun connect(input: String) {
        val origin = serverOrigin(input, BuildConfig.DEBUG)
        val next = withContext(Dispatchers.IO) { CairnApi(origin, vault) }
        val wasBusy = state.value.busy
        connection?.closeStreams()
        accountConnection?.closeStreams()
        clearDrafts()
        accountConnection = next
        emailChallenge = null

        mutable.update {
            if (connection != null && it.origin == origin.toString() && it.session.authenticated)
                it.copy(busy = true)
            else Workspace(busy = true, origin = origin.toString())
        }

        try {
            val session =
                try {
                    next.get<Session>("/account/session")
                } catch (error: IOException) {
                    if (error is ApiException && error.status == 401) {
                        next.clearSession()
                        Session()
                    } else if (next.hasSession && (error !is ApiException || error.status >= 500)) {
                        mutable.update {
                            it.copy(
                                restoringSession = true,
                                error =
                                    "Beacon est injoignable. Réessayez lorsque le réseau est disponible.",
                            )
                        }
                        return
                    } else throw error
                }
            next.csrf = session.csrf.orEmpty()
            if (session.authenticated) openAccount(session)
            else {
                transitionScope(forgetSession = true, scheduleNotifications = false)
                connection = null
                mutable.update {
                    Workspace(busy = true, origin = origin.toString(), session = session)
                }
            }
        } finally {
            mutable.update { it.copy(ready = true, busy = wasBusy) }
        }
    }

    suspend fun retrySession() {
        connect(state.value.origin)
    }

    private var signInJob: kotlinx.coroutines.Job? = null

    suspend fun accountOptions(): AccountOptions =
        checkNotNull(accountConnection).get("/account/options")

    suspend fun accountMethods(): AccountMethods =
        checkNotNull(accountConnection).get("/account/methods")

    suspend fun signIn(provider: AccountProvider, credentials: CairnCredentials) {
        val job = kotlinx.coroutines.currentCoroutineContext()[kotlinx.coroutines.Job]
        signInJob = job
        mutable.update { it.copy(signingIn = true) }
        try {
            val signIn = CairnAccountSignIn(checkNotNull(accountConnection), credentials)
            val session =
                when (provider) {
                    AccountProvider.Google -> signIn.google()
                    AccountProvider.GitHub -> signIn.github()
                    AccountProvider.Passkey -> signIn.passkey()
                }
            openAccount(session)
        } finally {
            if (signInJob === job) signInJob = null
            mutable.update { it.copy(signingIn = false) }
        }
    }

    fun cancelSignIn() {
        signInJob?.cancel()
    }

    suspend fun createAccountPasskey(label: String, credentials: CairnCredentials) =
        CairnAccountSignIn(checkNotNull(accountConnection), credentials).createPasskey(label)

    suspend fun confirmAccountPasskey(credentials: CairnCredentials) =
        CairnAccountSignIn(checkNotNull(accountConnection), credentials).confirmPasskey()

    suspend fun accountConfirmationCode(): EmailChallenge =
        checkNotNull(accountConnection)
            .send(
                "POST",
                "/account/email-code",
                body("email" to checkNotNull(state.value.session.account).email),
            )

    suspend fun confirmAccountEmail(challenge: String, code: String) {
        checkNotNull(accountConnection)
            .request(
                "POST",
                "/account/reauth/email",
                body("challenge" to challenge, "code" to code.trim()),
            )
    }

    suspend fun removeAccountMethod(id: String) {
        checkNotNull(accountConnection).request("POST", "/account/methods/remove", body("id" to id))
    }

    suspend fun setNativeNotifications(enabled: Boolean) {
        val registrar = NativeDeviceRegistrar(getApplication(), vault, beaconOrigin)
        if (enabled) registrar.enable() else registrar.disable()
    }

    suspend fun openNotification(cacheScope: String, accountId: String): Boolean {
        val current = state.value
        if (
            !current.session.authenticated ||
                (accountId.isNotBlank() && current.session.account?.id != accountId)
        )
            return false
        val installationId = cacheScope.removePrefix(current.origin)
        if (cacheScope != current.origin + installationId || installationId.isBlank()) return false
        refreshInstallations()
        val installation =
            state.value.session.installations.find { it.id == installationId } ?: return false
        selectInstallation(installation.id)
        return true
    }

    suspend fun requestEmailCode(email: String) {
        val target = checkNotNull(accountConnection) { "Beacon n’est pas configuré." }
        val normalizedEmail = email.trim().lowercase(java.util.Locale.ROOT)
        val request =
            target.send<EmailChallenge>(
                "POST",
                "/account/email-code",
                body("email" to normalizedEmail),
            )
        emailChallenge = request.challenge
        mutable.update { it.copy(emailForCode = normalizedEmail) }
    }

    fun changeEmail() {
        emailChallenge = null
        mutable.update { it.copy(emailForCode = null, error = null) }
    }

    suspend fun verifyEmailCode(code: String) {
        val target = checkNotNull(accountConnection)
        val challenge = checkNotNull(emailChallenge) { "Demandez un nouveau code par e-mail." }
        val session =
            target.send<Session>(
                "POST",
                "/account/verify",
                body("challenge" to challenge, "code" to code.trim()),
            )
        openAccount(session)
    }

    private suspend fun openAccount(session: Session) {
        require(session.authenticated && session.account != null && !session.csrf.isNullOrBlank()) {
            "La session du compte Cairn est invalide. Reconnectez-vous."
        }
        val account = checkNotNull(accountConnection)
        account.csrf = session.csrf.orEmpty()
        emailChallenge = null
        val saved = preferences.lastInstallation(state.value.origin, session.account.id)
        val installation =
            session.installations.find { it.id == saved } ?: session.installations.firstOrNull()
        if (installation != null) openInstallation(session, installation)
        else {
            transitionScope(
                notificationScope = "",
                scheduleNotifications = notifications.enabled.first(),
            )
            connection = null
            mutable.update {
                Workspace(ready = true, busy = it.busy, origin = it.origin, session = session)
            }
        }
    }

    suspend fun selectInstallation(id: String) {
        val current = state.value
        val installation = current.session.installations.first { it.id == id }
        if (current.installation == installation) return
        openInstallation(current.session, installation)
    }

    private suspend fun openInstallation(session: Session, installation: Installation) {
        val current = state.value
        val account = checkNotNull(accountConnection)
        mutable.update { it.copy(busy = true) }

        try {
            val accountId = checkNotNull(session.account).id
            val scope = "${current.origin}:$accountId:${installation.id}:${installation.role.value}"
            transitionScope(notificationScope = scope, csrf = session.csrf, clearStorage = false)

            preferences.selectInstallation(current.origin, accountId, installation.id)
            val next =
                withContext(Dispatchers.IO) {
                    CairnApi(account.origin, vault, installationId = installation.id).also {
                        it.csrf = account.csrf
                    }
                }
            connection = next
            routeCollection = viewModelScope.launch {
                next.transport.route.collect { route ->
                    if (connection === next) mutable.update { it.copy(transportRoute = route) }
                }
            }
            mutable.update {
                Workspace(
                    ready = true,
                    origin = current.origin,
                    session = session,
                    installation = installation,
                    busy = true,
                )
            }

            if (installation.online) {
                refresh()
                if (connection === next) next.startDirect(getApplication())
            }
            schedule(getApplication(), notifications.enabled.first())
        } finally {
            mutable.update { it.copy(busy = current.busy) }
        }
    }

    suspend fun refreshInstallations() {
        val account = accountConnection ?: return
        if (!state.value.session.authenticated) return
        val currentConnection = connection
        val wasBusy = state.value.busy
        val installations = account.get<List<Installation>>("/installations")
        if (
            accountConnection !== account ||
                connection !== currentConnection ||
                state.value.busy != wasBusy ||
                !state.value.session.authenticated
        )
            return
        val previous = state.value.installation
        val selected = installations.find { it.id == previous?.id }
        mutable.update { it.copy(session = it.session.copy(installations = installations)) }

        when {
            selected == null || selected.role != previous?.role -> openAccount(state.value.session)
            else -> {
                mutable.update { it.copy(installation = selected) }
                if (selected.online && !previous.online) {
                    refresh()
                    if (connection === currentConnection) connection?.startDirect(getApplication())
                }
                if (!selected.online) connection?.closeStreams()
            }
        }
    }

    suspend fun logout() {
        emailChallenge = null
        mutable.update { it.copy(signingOut = true) }
        transitionScope(scheduleNotifications = false)
        try {
            try {
                NativeDeviceRegistrar(getApplication(), vault, beaconOrigin).disable()
            } catch (error: CancellationException) {
                throw error
            } catch (_: Exception) {
                notifications.setEnabled(false)
            }
            checkNotNull(accountConnection).request("POST", "/account/logout")
        } finally {
            withContext(kotlinx.coroutines.NonCancellable) {
                try {
                    kotlinx.coroutines.withTimeout(5000) {
                        AndroidCairnCredentials(getApplication()).clear()
                    }
                } catch (_: Exception) {
                    /* Local logout wins over unavailable credential providers. */
                }
                transitionScope(clearStorage = false, forgetSession = true)
                connection = null
                mutable.update { Workspace(ready = true, origin = it.origin, busy = it.busy) }
            }
        }
    }

    suspend fun refresh() = coroutineScope {
        val target = api
        val owner = state.value.isOwner
        val agents = async { target.get<List<Agent>>("/agents") }
        val projects = async { target.get<List<Project>>("/projects") }
        val tasks = async { target.get<List<Task>>("/tasks") }
        val skills = async { target.get<List<Skill>>("/skills") }
        val mcps = async { if (owner) target.get<List<Mcp>>("/mcps") else emptyList() }
        val catalogs = codingAgents.associate { agent ->
            agent.provider to
                async {
                    try {
                        target.get<ModelCatalog>("/${agent.provider}/models")
                    } catch (e: Exception) {
                        if (e is CancellationException || (e is ApiException && e.status == 401))
                            throw e
                        state.value
                            .modelCatalog(agent.provider)
                            .copy(
                                stale = true,
                                error = "Catalogue temporairement indisponible.",
                            )
                    }
                }
        }
        val overview = async { target.get<Overview>("/overview") }
        val updated =
            Workspace(
                agents = agents.await(),
                projects = projects.await(),
                tasks = tasks.await(),
                skills = skills.await(),
                overview = overview.await(),
                mcps = mcps.await(),
                modelCatalogs = catalogs.mapValues { (_, catalog) -> catalog.await() },
            )
        if (connection === target && state.value.session.authenticated)
            mutable.update {
                it.copy(
                    agents = updated.agents,
                    projects = updated.projects,
                    tasks = updated.tasks,
                    skills = updated.skills,
                    overview = updated.overview,
                    mcps = updated.mcps,
                    modelCatalogs = updated.modelCatalogs,
                )
            }
    }

    suspend fun refreshAgentPortraits() {
        val target = api
        val before = state.value.agents.associate { it.id to it.avatar }
        val portraits = target.get<List<Agent>>("/agents").associateBy { it.id }
        if (connection === target && state.value.session.authenticated)
            mutable.update { current ->
                current.copy(
                    agents =
                        current.agents.map {
                            if (it.avatar == before[it.id])
                                it.copy(avatar = portraits[it.id]?.avatar)
                            else it
                        }
                )
            }
    }

    suspend fun refreshModels(provider: String) {
        codingAgent(provider)
        val target = api
        try {
            val catalog = target.get<ModelCatalog>("/$provider/models")
            // A request started before sign-out or a server change must not restore old data.
            if (connection === target && state.value.session.authenticated)
                mutable.update {
                    it.copy(modelCatalogs = it.modelCatalogs + (provider to catalog))
                }
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            if (connection !== target || !state.value.session.authenticated) return
            if (e is ApiException && e.status == 401) {
                report(e)
                return
            }
            mutable.update {
                val cached = it.modelCatalog(provider)
                val unavailable =
                    cached.copy(
                        stale = true,
                        error = "Catalogue temporairement indisponible. Réessayez.",
                    )
                it.copy(modelCatalogs = it.modelCatalogs + (provider to unavailable))
            }
        }
    }

    suspend fun save(kind: String, id: String, value: JsonElement) {
        api.request(
            if (id.isEmpty()) "POST" else "PUT",
            "/$kind" + if (id.isEmpty()) "" else "/${segment(id)}",
            value,
        )
        refresh()
        mutable.update { it.copy(notice = "Modifications enregistrées") }
    }

    suspend fun delete(path: String) {
        api.request("DELETE", path)
        refresh()
        mutable.update { it.copy(notice = "Suppression effectuée") }
    }
}

data class ChatDraft(
    val text: String = "",
    val model: String = "",
    val reasoning: String = "",
    val attachments: List<DraftAttachment> = emptyList(),
    val editing: String? = null,
    val submissionId: String = "",
    val submissionKey: String = "",
    val provider: String = "",
    val rejected: List<RejectedAttachment> = emptyList(),
    /** The next messages are plan turns until the user turns it off or implements a plan. */
    val plan: Boolean = false,
)
