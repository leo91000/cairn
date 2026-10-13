package build.cairn.app.data

/** Metadata shared by account screens, model loading and coding-agent pickers. */
data class CodingAgent(
    val provider: String,
    val vendor: String,
    val subscription: String,
    val label: String,
)

val codingAgents =
    listOf(
        CodingAgent("codex", "OpenAI", "Compte ChatGPT", "Codex"),
        CodingAgent("claude", "Anthropic", "Compte Claude", "Claude Code"),
    )

fun codingAgent(provider: String): CodingAgent =
    requireNotNull(codingAgents.find { it.provider == provider }) {
        "Unknown coding agent: $provider"
    }
