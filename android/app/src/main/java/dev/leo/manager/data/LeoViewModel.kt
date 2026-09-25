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
)

class LeoViewModel
@JvmOverloads
constructor(
    application: Application,
    private val vault: SessionVault = KeystoreSessionVault(application),
) : AndroidViewModel(application) {
    val historyCache = HistoryCache.encrypted(application)
    private val retentionPreferences = application.getSharedPreferences("conversation-cache", 0)

    suspend fun acceptCacheRevision(revision: String?) {
        if (
            revision != null && retentionPreferences.getString(state.value.origin, null) != revision
        ) {
            historyCache.clear()
            retentionPreferences.edit().putString(state.value.origin, revision).apply()
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
    val api: LeoApi
        get() = checkNotNull(connection) { "Connectez-vous à votre serveur." }

    init {
        perform {
            val origin = preferences.origin.first()
            if (origin.isNotBlank()) connect(origin)
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
        if (error is ApiException && error.status == 401) {
            clearDrafts()
            viewModelScope.launch { historyCache.clear() }
            connection?.clearSession()
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

    suspend fun connect(input: String) {
        val origin = serverOrigin(input, BuildConfig.DEBUG)
        val next = withContext(Dispatchers.IO) { LeoApi(origin, vault) }
        val session = next.get<Session>("/session")
        next.csrf = session.csrf
        connection?.closeStreams()
        if (
            !session.authenticated ||
                (connection != null &&
                    (connection?.origin != next.origin || connection?.csrf != next.csrf))
        )
            historyCache.clear()
        connection = next
        preferences.setOrigin(origin.toString())
        mutable.update {
            Workspace(ready = true, busy = it.busy, origin = origin.toString(), session = session)
        }
        if (session.authenticated) {
            schedule(getApplication(), notifications.enabled.first())
            refresh()
        }
    }

    suspend fun login(password: String, setupToken: String) {
        val session =
            api.send<Session>(
                "POST",
                if (state.value.session.setupRequired) "/setup" else "/login",
                body("password" to password, "setupToken" to setupToken),
            )
        api.csrf = session.csrf
        mutable.update { it.copy(session = session) }
        schedule(getApplication(), notifications.enabled.first())
        refresh()
    }

    suspend fun logout() {
        clearDrafts()
        mutable.update { it.copy(signingOut = true) }
        historyCache.clear()
        try {
            api.closeStreams()
            schedule(getApplication(), false)
            api.request("POST", "/logout")
            withContext(Dispatchers.IO) { api.clearSession() }
            mutable.update { Workspace(ready = true, origin = it.origin) }
        } finally {
            mutable.update { it.copy(signingOut = false) }
        }
    }

    suspend fun forget() {
        clearDrafts()
        historyCache.clear()
        withContext(Dispatchers.IO) { connection?.clearSession() }
        connection = null
        schedule(getApplication(), false)
        notifications.setSeen(emptySet())
        preferences.setOrigin("")
        mutable.value = Workspace(ready = true)
    }

    suspend fun refresh() = coroutineScope {
        val agents = async { api.get<List<Agent>>("/agents") }
        val projects = async { api.get<List<Project>>("/projects") }
        val tasks = async { api.get<List<Task>>("/tasks") }
        val skills = async { api.get<List<Skill>>("/skills") }
        val mcps = async { api.get<List<Mcp>>("/mcps") }
        val models = async {
            try {
                api.get<ModelCatalog>("/codex/models")
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
                api.get<ModelCatalog>("/claude/models")
            } catch (e: Exception) {
                if (e is CancellationException || (e is ApiException && e.status == 401)) throw e
                state.value.claudeModels.copy(
                    stale = true,
                    error = "Catalogue Claude temporairement indisponible.",
                )
            }
        }
        val overview = async { api.get<Overview>("/overview") }
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
