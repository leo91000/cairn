package build.cairn.app.data

/** Metadata shared by account screens, model loading and coding-agent pickers. */
data class CodingAgent(
    val provider: String,
    val vendor: String,
    val subscription: String,
    val label: String,
    val apiKeys: Boolean = false,
    val requiresModel: Boolean = false,
    /** Runs try accounts in the user's order instead of by remaining capacity. */
    val ordered: Boolean = false,
)

val codingAgents =
    listOf(
        CodingAgent("codex", "OpenAI", "Compte ChatGPT", "Codex"),
        CodingAgent("claude", "Anthropic", "Compte Claude", "Claude Code"),
        CodingAgent(
            "opencode",
            "Fournisseurs de modèles",
            "Clé API",
            "OpenCode",
            apiKeys = true,
            requiresModel = true,
            ordered = true,
        ),
    )

fun codingAgent(provider: String): CodingAgent =
    requireNotNull(codingAgents.find { it.provider == provider }) {
        "Unknown coding agent: $provider"
    }
