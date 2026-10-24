package build.cairn.app.data

// A speed is independent of the model and the reasoning level: it is kept when either changes.
// Only Codex lists speeds, per model and account, in its catalog. Mirrors the web app's speeds.ts.

/** Codex's standard speed, chosen explicitly. An empty speed inherits. */
const val STANDARD_SPEED = "standard"

/** Where OpenAI documents how each speed uses plan limits and credits. */
const val SPEED_USAGE_URL = "https://learn.chatgpt.com/docs/agent-configuration/speed"

/** The speed a run uses: the chosen one, else the inherited one, else the model's own default. */
fun effectiveSpeed(speed: String, inherited: String, model: CodingAgentModel?): String =
    speed
        .ifBlank { inherited }
        .ifBlank { model?.defaultServiceTier.orEmpty() }
        .ifBlank {
            STANDARD_SPEED
        }

/**
 * Whether the model offers [speed]. A catalog without speeds, cached before they were listed or
 * from an older Codex, cannot tell: the run then leaves the check to Codex.
 */
fun offersSpeed(model: CodingAgentModel?, speed: String): Boolean {
    val tiers = model?.serviceTiers
    if (speed.isBlank() || speed == STANDARD_SPEED || tiers == null) return true
    return tiers.any { it.id == speed }
}

/** The speed section shows for Codex models that list speeds, or to keep a saved choice. */
fun showsSpeed(provider: String, model: CodingAgentModel?, speed: String) =
    provider == "codex" && (!model?.serviceTiers.isNullOrEmpty() || speed.isNotBlank())

/** Codex's own name for a speed, in its catalog; a readable id otherwise. */
fun speedLabel(value: String, tiers: List<ServiceTier> = emptyList()): String =
    when {
        value == STANDARD_SPEED -> "Standard"
        value.isBlank() -> "Par défaut"
        else ->
            tiers.find { it.id == value }?.name?.ifBlank { null }
                ?: value.replace('_', ' ').replaceFirstChar { it.uppercase() }
    }
