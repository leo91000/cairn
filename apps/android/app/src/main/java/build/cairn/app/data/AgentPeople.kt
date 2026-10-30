package build.cairn.app.data

import kotlinx.serialization.Serializable

// Which agents an account may use (ADR-0075). The server enforces it; the app only avoids offering
// what would be refused. A null list means every agent: the owner, or not loaded yet.

/** A member of the installation, as the Beacon lists them to its owner. */
@Serializable data class InstallationMember(val id: String, val email: String = "")

@Serializable data class InstallationSharing(val members: List<InstallationMember> = emptyList())

fun canUseAgent(usable: List<String>?, agentId: String?): Boolean =
    usable == null || (agentId != null && agentId in usable)

/**
 * A conversation is directed through its agent. A delegated task may also be directed through the
 * agent of its requesting conversation, while that conversation exists.
 */
fun canDirectChat(usable: List<String>?, chat: Chat, chats: List<Chat>): Boolean {
    if (canUseAgent(usable, chat.agentId)) return true
    val requesting = chat.requester?.let { requester -> chats.find { it.id == requester.chatId } }
    return requesting != null && canUseAgent(usable, requesting.agentId)
}

/**
 * Who may direct a conversation the viewer can only follow: people with access to its agent and,
 * for a delegated task whose requesting conversation still exists, to that conversation's agent.
 */
fun readOnlyNotice(chat: Chat, chats: List<Chat>): String {
    val own = chat.agentName.ifBlank { "son agent" }
    val requesting = chat.requester?.let { requester -> chats.find { it.id == requester.chatId } }
    val who =
        if (requesting == null) "Seules les personnes ayant accès à $own"
        else
            "Seules les personnes ayant accès à $own ou à ${requesting.agentName.ifBlank { "l’agent de la conversation qui l’a déléguée" }}"
    return "$who peuvent la poursuivre ou répondre à ses questions, et seules celles ayant accès à $own peuvent la supprimer. Vous pouvez toujours la mettre en pause ou l’arrêter. Demandez l’accès au propriétaire de l’installation."
}

/** The agent a new conversation starts with: Main when allowed, otherwise the first usable one. */
fun defaultAgent(usable: List<String>?, agents: List<Agent>): String =
    if (canUseAgent(usable, MAIN_AGENT_ID)) MAIN_AGENT_ID
    else agents.firstOrNull { canUseAgent(usable, it.id) }?.id.orEmpty()

/** One member and whether they may use the agent; [known] is false for a former member. */
data class AgentAccessRow(
    val accountId: String,
    val email: String,
    val known: Boolean,
    val allowed: Boolean,
)

/** Every member once, then entries a former member still holds until the next check. */
fun agentAccessRows(
    members: List<InstallationMember>,
    allowed: List<String>,
): List<AgentAccessRow> =
    members.map { AgentAccessRow(it.id, it.email, known = true, allowed = it.id in allowed) } +
        allowed
            .filter { account -> members.none { it.id == account } }
            .map { AgentAccessRow(it, "Ancien membre", known = false, allowed = true) }

/**
 * Applies one member's change from a response and keeps the other rows as shown, so responses to
 * several quick changes cannot undo each other when they arrive out of order.
 */
fun withAccess(allowed: List<String>, accountId: String, accounts: List<String>): List<String> =
    when {
        accountId !in accounts -> allowed - accountId
        accountId in allowed -> allowed
        else -> allowed + accountId
    }

fun agentAccessSummary(allowed: Int): String =
    when (allowed) {
        0 -> "Vous seul"
        1 -> "Vous et 1 membre"
        else -> "Vous et $allowed membres"
    }
