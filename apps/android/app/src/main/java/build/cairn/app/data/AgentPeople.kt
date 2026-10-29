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

fun agentAccessSummary(allowed: Int): String =
    when (allowed) {
        0 -> "Vous seul"
        1 -> "Vous et 1 membre"
        else -> "Vous et $allowed membres"
    }
