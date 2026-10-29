package build.cairn.app.data

/** Form feedback; the generated contracts and server own wire validation. */
val QuestionField.supportsAnswers: Boolean
    get() =
        mode in setOf("single", "multiple") &&
            options.all { it.id.isNotEmpty() } &&
            options.map { it.id }.distinct().size == options.size

fun QuestionField.accepts(selected: List<String>, comment: String): Boolean {
    if (!supportsAnswers) return false
    if (selected.isEmpty()) return comment.isNotBlank()
    val maximum = if (mode == "single") 1 else maxChoices ?: options.size
    return selected.size >= (minChoices ?: 1) &&
        selected.size <= maximum &&
        selected.distinct().size == selected.size &&
        selected.all { id -> options.any { it.id == id } }
}
