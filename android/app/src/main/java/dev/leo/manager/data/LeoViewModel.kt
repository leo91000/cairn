package dev.leo.manager.data

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.leo.manager.BuildConfig
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

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
    val mcps: List<Mcp> = emptyList(),
    val models: ModelCatalog = ModelCatalog(),
    val claudeModels: ModelCatalog = ModelCatalog(),
    val error: String? = null,
    val notice: String? = null,
    val agents: List<Agent> = emptyList(),
    val projects: List<Project> = emptyList(),
    val tasks: List<Task> = emptyList(),
    val skills: List<Skill> = emptyList(),
    val overview: Overview = Overview(),
) {
    val isOwner: Boolean
        get() = installation?.role == "owner"
}

class LeoViewModel
@JvmOverloads
constructor(
    application: Application,
    private val vault: SessionVault = KeystoreSessionVault(application),
    private val officialOrigin: String = BuildConfig.OFFICIAL_SERVICE_ORIGIN,
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
    private var connection: LeoApi? = null
    private var accountConnection: LeoApi? = null
    private var emailChallenge: String? = null
    val api: LeoApi
        get() = checkNotNull(connection) { "Choisissez une installation." }

    init {
        perform {
            val origin = officialOrigin
            if (origin.isNotBlank()) connect(origin)
            else
                notify(
                    "Le service officiel n’est pas configuré dans cette version de l’application."
                )
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
            clearDrafts()
            viewModelScope.launch { historyCache.clear() }
            connection?.closeStreams()
            connection?.clearSession()
            accountConnection?.clearSession()
            connection = null
            schedule(getApplication(), false)
            mutable.update {
                Workspace(
                    ready = true,
                    origin = it.origin,
                    error = "Session expirée. Reconnectez-vous.",
                )
            }
        } else
            mutable.update { it.copy(error = error.message ?: "Connexion impossible. Réessayez.") }
    }

    fun perform(block: suspend LeoViewModel.() -> Unit) {
        if (mutable.value.busy) return
        mutable.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            try {
                block()
            } catch (e: Exception) {
                report(e)
            } finally {
                mutable.update { it.copy(busy = false, ready = true) }
            }
        }
    }

    // The shipped app supplies only BuildConfig.OFFICIAL_SERVICE_ORIGIN. Tests inject an HTTP
    // fixture.
    internal suspend fun connect(input: String) {
        val origin = serverOrigin(input, BuildConfig.DEBUG)
        val next = withContext(Dispatchers.IO) { LeoApi(origin, vault) }
        val wasBusy = state.value.busy
        connection?.closeStreams()
        accountConnection?.closeStreams()
        clearDrafts()
        accountConnection = next
        emailChallenge = null
        mutable.update { Workspace(busy = true, origin = origin.toString()) }
        try {
            val session = next.get<Session>("/account/session")
            next.csrf = session.csrf.orEmpty()
            if (session.authenticated) openAccount(session)
            else {
                historyCache.clear()
                next.clearSession()
                connection = null
                schedule(getApplication(), false)
                mutable.update {
                    Workspace(busy = true, origin = origin.toString(), session = session)
                }
            }
        } finally {
            mutable.update { it.copy(ready = true, busy = wasBusy) }
        }
    }

    suspend fun requestEmailCode(email: String) {
        val target = checkNotNull(accountConnection) { "Le service officiel n’est pas configuré." }
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
            "La session du compte Leo est invalide. Reconnectez-vous."
        }
        val account = checkNotNull(accountConnection)
        account.csrf = session.csrf.orEmpty()
        emailChallenge = null
        val saved = preferences.lastInstallation(state.value.origin, session.account.id)
        val installation =
            session.installations.find { it.id == saved } ?: session.installations.firstOrNull()
        if (installation != null) openInstallation(session, installation)
        else {
            connection?.closeStreams()
            historyCache.clear()
            connection = null
            schedule(getApplication(), false)
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
        val previous = connection
        val account = checkNotNull(accountConnection)
        mutable.update { it.copy(busy = true) }
        try {
            previous?.closeStreams()
            clearDrafts()
            val accountId = checkNotNull(session.account).id
            val scope = "${current.origin}:$accountId:${installation.id}:${installation.role}"
            val scopeChanged = notifications.selectScope(scope)
            val sessionChanged = previous != null && previous.csrf != session.csrf
            if (scopeChanged || sessionChanged) {
                historyCache.clear()
                schedule(getApplication(), false)
            }
            preferences.selectInstallation(current.origin, accountId, installation.id)
            val next =
                withContext(Dispatchers.IO) {
                    LeoApi(account.origin, vault, installationId = installation.id).also {
                        it.csrf = account.csrf
                    }
                }
            connection = next
            mutable.update {
                Workspace(
                    ready = true,
                    origin = current.origin,
                    session = session,
                    installation = installation,
                    busy = true,
                )
            }
            if (installation.online) refresh()
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
            selected == null || selected.role != previous?.role -> {
                connection?.closeStreams()
                connection = null
                clearDrafts()
                historyCache.clear()
                mutable.update {
                    Workspace(
                        ready = true,
                        origin = it.origin,
                        session = it.session,
                        busy = it.busy,
                    )
                }
                val next = selected ?: installations.firstOrNull()
                if (next != null) selectInstallation(next.id) else schedule(getApplication(), false)
            }
            else -> {
                mutable.update { it.copy(installation = selected) }
                if (selected.online && previous?.online != true) refresh()
                if (!selected.online) connection?.closeStreams()
            }
        }
    }

    suspend fun logout() {
        clearDrafts()
        emailChallenge = null
        mutable.update { it.copy(signingOut = true) }
        historyCache.clear()
        connection?.closeStreams()
        schedule(getApplication(), false)
        try {
            checkNotNull(accountConnection).request("POST", "/account/logout")
        } finally {
            withContext(Dispatchers.IO) {
                connection?.clearSession()
                accountConnection?.clearSession()
            }
            connection = null
            mutable.update { Workspace(ready = true, origin = it.origin, busy = it.busy) }
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
        val models = async {
            try {
                target.get<ModelCatalog>("/codex/models")
            } catch (e: Exception) {
                if (e is CancellationException || (e is ApiException && e.status == 401)) throw e
                state.value.models.copy(
                    stale = true,
                    error = "Catalogue temporairement indisponible.",
                )
            }
        }
        val claudeModels = async {
            try {
                target.get<ModelCatalog>("/claude/models")
            } catch (e: Exception) {
                if (e is CancellationException || (e is ApiException && e.status == 401)) throw e
                state.value.claudeModels.copy(
                    stale = true,
                    error = "Catalogue Claude temporairement indisponible.",
                )
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
                models = models.await(),
                claudeModels = claudeModels.await(),
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
                    models = updated.models,
                    claudeModels = updated.claudeModels,
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
        require(provider in listOf("codex", "claude"))
        val target = api
        try {
            val catalog = target.get<ModelCatalog>("/$provider/models")
            // A request started before sign-out or a server change must not restore old data.
            if (connection === target && state.value.session.authenticated)
                mutable.update {
                    if (provider == "claude") it.copy(claudeModels = catalog)
                    else it.copy(models = catalog)
                }
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            if (connection !== target || !state.value.session.authenticated) return
            if (e is ApiException && e.status == 401) {
                report(e)
                return
            }
            mutable.update {
                val cached = if (provider == "claude") it.claudeModels else it.models
                val unavailable =
                    cached.copy(
                        stale = true,
                        error = "Catalogue temporairement indisponible. Réessayez.",
                    )
                if (provider == "claude") it.copy(claudeModels = unavailable)
                else it.copy(models = unavailable)
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
)
