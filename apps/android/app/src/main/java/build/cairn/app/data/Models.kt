package build.cairn.app.data

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.encodeToJsonElement

@Serializable
data class Session(
    val authenticated: Boolean = false,
    val csrf: String? = null,
    val account: CairnAccount? = null,
    val installations: List<Installation> = emptyList(),
)

@Serializable data class EmailChallenge(val challenge: String)

@Serializable data class CairnAccount(val id: String, val email: String)

@Serializable
enum class InstallationRole(val value: String, val label: String) {
    @SerialName("owner") Owner("owner", "Propriétaire"),
    @SerialName("member") Member("member", "Membre"),
}

@Serializable
data class Installation(
    val id: String,
    val name: String,
    val role: InstallationRole = InstallationRole.Member,
    val online: Boolean = false,
    val updateRequired: Boolean = false,
)

const val DRAFT_AGENT_ID = "00000000-0000-4000-8000-000000000000"

const val MAIN_AGENT_ID = "00000000-0000-4000-8000-000000000001"

typealias AccessPolicy = build.cairn.contracts.AccessPolicy

typealias AgentPortrait = build.cairn.contracts.AgentAvatar

typealias Agent = build.cairn.contracts.Agent

typealias Project = build.cairn.contracts.Project

typealias Task = build.cairn.contracts.Mission

typealias ScheduleFailure = build.cairn.contracts.ScheduleFailure

fun Task.settings(): build.cairn.contracts.MissionSettings =
    build.cairn.contracts.MissionSettings(
        name = name,
        prompt = prompt,
        agentId = agentId,
        projectId = projectId,
        skills = skills,
        tags = tags,
        cron = cron,
        timezone = timezone,
        enabled = enabled,
        archived = archived,
        worktree = worktree,
    )

fun Task.runTaskSnapshot(): build.cairn.contracts.RunTaskSnapshot =
    wireJson.decodeFromJsonElement(
        build.cairn.contracts.RunTaskSnapshot.serializer(),
        wireJson.encodeToJsonElement(build.cairn.contracts.Mission.serializer(), this),
    )

typealias Skill = build.cairn.contracts.Skill

typealias SnapshotSkill = build.cairn.contracts.RunSkill

typealias Snapshot = build.cairn.contracts.RunSnapshot

typealias RunRetryCause = build.cairn.contracts.Cause

typealias RunRetry = build.cairn.contracts.Retry

typealias Run = build.cairn.contracts.Run

val Run.active
    get() = status in listOf("running", "queued", "waiting")

val Run.title
    get() = taskName.ifBlank { snapshot?.task?.name.orEmpty() }.ifBlank { "Exécution" }

typealias RunEvent = build.cairn.contracts.RunEvent

@Serializable
data class Grant(
    val id: String,
    val label: String = "",
    val scopes: List<String> = emptyList(),
    val clientId: String = "",
)

typealias Occurrences = build.cairn.contracts.SchedulePreviewOutput

@Serializable data class FileContent(val content: String)

@Serializable data class TokenResult(val token: String)

@Serializable data class OAuthClient(val client_name: String = "")

@Serializable data class ConsentPreview(val client: OAuthClient, val scopes: List<String>)

@Serializable data class ConsentResult(val redirect: String)

typealias TaskOutcome = build.cairn.boundary.TaskOutcome

typealias RunRequester = build.cairn.contracts.RunRequester

/** A dismissed outcome has no [until]; a snoozed one asks again then. */
typealias OutcomeDismissal = build.cairn.contracts.OutcomeDismissal

typealias ChatExecution = build.cairn.boundary.ChatExecution
