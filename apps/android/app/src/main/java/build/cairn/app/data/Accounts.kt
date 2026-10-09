package build.cairn.app.data

import kotlinx.serialization.Serializable

/** Coding-agent accounts, as served by /api/accounts. */
@Serializable
data class AccountsView(
    val accounts: List<Account> = emptyList(),
    val signIn: AccountSignIn? = null,
    /** Coding agents whose runs wait for the user to connect, reconnect or resume an account. */
    val required: List<String> = emptyList(),
)

@Serializable
data class AccountWindow(
    val id: String,
    val label: String = "",
    val usedPercent: Double = 0.0,
    val resetsAt: Long? = null,
    val durationMins: Int? = null,
    /** Empty when the window limits every model. */
    val models: List<String> = emptyList(),
) {
    val remaining
        get() = (100.0 - usedPercent).coerceIn(0.0, 100.0)
}

@Serializable data class BankedResets(val available: Int = 0)

@Serializable
data class AccountUsage(
    val windows: List<AccountWindow> = emptyList(),
    val checkedAt: Long? = null,
    val error: String? = null,
    val resets: BankedResets? = null,
) {
    /** Windows limiting every model, shortest first. */
    val general
        get() = windows.filter { it.models.isEmpty() }.sortedBy { it.durationMins ?: 0 }
}

@Serializable
data class Account(
    val id: String,
    val provider: String = "codex",
    val name: String,
    val enabled: Boolean = true,
    val email: String? = null,
    val plan: String? = null,
    val state: String = "pending",
    /** signIn, reconnect, paused, unavailable, waiting, full, next, low or ready. */
    val status: String = "ready",
    val error: String = "",
    val resetError: String = "",
    val maxConcurrentRuns: Int = 4,
    val activeRunIds: List<String> = emptyList(),
    val usage: AccountUsage? = null,
    val remainingPercent: Double? = null,
    val resetsAt: Long? = null,
    val stale: Boolean = true,
)

@Serializable
data class AccountSignIn(
    val accountId: String = "",
    val provider: String = "codex",
    val state: String = "pending",
    val phase: String = "starting",
    val url: String? = null,
    /** A code to enter on OpenAI's page. */
    val code: String? = null,
    /** Whether Anthropic's page shows a code to paste back here. */
    val acceptsCode: Boolean = false,
    val error: String? = null,
)
