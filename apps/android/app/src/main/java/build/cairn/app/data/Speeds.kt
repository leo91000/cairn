package build.cairn.app.data

// A speed is independent of the model and the reasoning level: it is kept when either changes.
// Only Codex lists speeds, per model and account, in its catalog. Mirrors the web app's speeds.ts.

/** Codex's standard speed, chosen explicitly. An empty speed inherits. */
const val STANDARD_SPEED = "standard"

/** Where OpenAI documents how each speed uses plan limits and credits. */
const val SPEED_USAGE_URL = "https://learn.chatgpt.com/docs/agent-configuration/speed"

/**
 * The speed a run uses: the chosen one, else the inherited one, else Standard. Codex's app server
 * does not apply a model's `defaultServiceTier`, so Cairn never claims it.
 */
fun effectiveSpeed(speed: String, inherited: String): String =
    speed.ifBlank { inherited }.ifBlank { STANDARD_SPEED }

/**
 * Whether the model offers [speed]. A catalog without speeds, cached before they were listed,
 * cannot tell: the turn then checks with Codex.
 */
fun offersSpeed(model: CodingAgentModel?, speed: String): Boolean {
    val tiers = model?.serviceTiers
    if (speed.isBlank() || speed == STANDARD_SPEED || tiers == null) return true
    return tiers.any { it.id == speed }
}

/** The speed section shows for Codex models that list speeds, or to keep a saved choice. */
fun showsSpeed(provider: String, model: CodingAgentModel?, speed: String) =
    provider == "codex" && (!model?.serviceTiers.isNullOrEmpty() || speed.isNotBlank())

/**
 * A queued or saved faster speed is a request, which each turn reports on, since Codex may answer
 * with its built-in list and an account's plan decides; one the model's catalog lacks says so. An
 * empty model is the catalog's default.
 */
fun speedNote(speed: String, model: String, models: List<CodingAgentModel>): String? {
    if (speed.isBlank()) return null
    // Codex's own tier names, such as Fast, keep their capital.
    val label = speedLabel(speed, models.flatMap { it.serviceTiers.orEmpty() })
    if (speed == STANDARD_SPEED) return "vitesse $label"
    val row = models.find { if (model.isEmpty()) it.isDefault else it.model == model }
    return if (row?.serviceTiers != null && !offersSpeed(row, speed))
        "vitesse $label demandée (non listée pour ce modèle)"
    else "vitesse $label demandée"
}

/** Codex's own name for a speed, in its catalog; a readable id otherwise. */
fun speedLabel(value: String, tiers: List<ServiceTier> = emptyList()): String =
    when {
        value == STANDARD_SPEED -> "Standard"
        value.isBlank() -> "Par défaut"
        else ->
            tiers.find { it.id == value }?.name?.ifBlank { null }
                ?: value.replace('_', ' ').replaceFirstChar { it.uppercase() }
    }
