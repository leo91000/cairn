package build.cairn.app.data

/** Coding-agent account wire models are generated from Rust. */
typealias AccountsView = build.cairn.contracts.CodingAgentAccounts

typealias AccountWindow = build.cairn.contracts.AccountWindow

typealias BankedResets = build.cairn.contracts.AccountResets

typealias AccountUsage = build.cairn.contracts.AccountUsage

typealias Account = build.cairn.contracts.CodingAgentAccount

typealias AccountSignIn = build.cairn.contracts.AccountSignIn

typealias ApiKeyProvider = build.cairn.contracts.ApiKeyProvider

val AccountWindow.remaining
    get() = (100.0 - (usedPercent ?: 0.0)).coerceIn(0.0, 100.0)
val AccountUsage.general
    get() = windows.filter { it.models.isEmpty() }.sortedBy { it.durationMins ?: 0 }

/** A typed refusal must remain an error in the owner UI rather than closing its sheet. */
fun build.cairn.contracts.ConnectAccountOutput.requireConnection() {
    when (this) {
        is build.cairn.contracts.ConnectAccountOutput.A,
        is build.cairn.contracts.ConnectAccountOutput.B -> Unit
        is build.cairn.contracts.ConnectAccountOutput.C -> error(message)
        is build.cairn.contracts.ConnectAccountOutput.D -> error(message)
    }
}

fun build.cairn.contracts.AccountMutationOutput.requireCompleted() {
    when (this) {
        is build.cairn.contracts.AccountMutationOutput.A -> Unit
        is build.cairn.contracts.AccountMutationOutput.B -> error(message)
    }
}
