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
    /** Cairn can enforce a plan turn's read-only boundary for this engine. */
    val planTurns: Boolean = false,
    /** Besides API keys, an account can sign in with the vendor's own login (Antigravity). */
    val accountSignIn: Boolean = false,
)

val codingAgents =
    listOf(
        CodingAgent("codex", "OpenAI", "Compte ChatGPT", "Codex", planTurns = true),
        CodingAgent("claude", "Anthropic", "Compte Claude", "Claude Code", planTurns = true),
        // OpenCode runs only in YOLO environments, without a sandbox Cairn could narrow.
        CodingAgent(
            "opencode",
            "Fournisseurs de modèles",
            "Clé API",
            "OpenCode",
            apiKeys = true,
            requiresModel = true,
            ordered = true,
        ),
        // Antigravity also requires YOLO access; its plan turns are bounded by Cairn's hook.
        CodingAgent(
            "antigravity",
            "Google",
            "Clé API ou compte Google",
            "Antigravity",
            apiKeys = true,
            // Like the web: an agent without a model uses the catalog's default.
            ordered = true,
            planTurns = true,
            accountSignIn = true,
        ),
    )

/** The `modelProvider` of an Antigravity account signed in with a Google account. */
const val GOOGLE_ACCOUNT = "google"

/**
 * Whether [account] signs in with the vendor's login rather than a key. An Antigravity account
 * still signing in has no model provider yet.
 */
fun signsIn(account: Account): Boolean =
    codingAgent(account.provider).let {
        !it.apiKeys ||
            (it.accountSignIn && (account.modelProvider ?: GOOGLE_ACCOUNT) == GOOGLE_ACCOUNT)
    }

/** Whether reconnecting [account] replaces its key instead of signing in again. */
fun replacesKey(account: Account): Boolean = !signsIn(account)

fun codingAgent(provider: String): CodingAgent =
    requireNotNull(codingAgents.find { it.provider == provider }) {
        "Unknown coding agent: $provider"
    }
