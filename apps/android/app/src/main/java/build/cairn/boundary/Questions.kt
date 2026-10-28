// Temporary A8 questionnaire boundary.
package build.cairn.boundary

import kotlinx.serialization.Serializable

@Serializable
data class QuestionOption(
    val label: String,
    val description: String = "",
    val id: String = "",
    val recommended: Boolean = false,
)

@Serializable
data class QuestionField(
    val id: String,
    val title: String,
    val secret: Boolean = false,
    val options: List<QuestionOption> = emptyList(),
    val mode: String = "single",
    val minChoices: Int? = null,
    val maxChoices: Int? = null,
) {
    val supportsAnswers: Boolean
        get() =
            mode in setOf("single", "multiple") &&
                options.all { it.id.isNotEmpty() } &&
                options.map { it.id }.distinct().size == options.size

    fun accepts(selected: List<String>, comment: String): Boolean {
        if (!supportsAnswers) return false
        if (selected.isEmpty()) return comment.isNotBlank()
        val maximum = if (mode == "single") 1 else maxChoices ?: options.size
        return selected.size >= (minChoices ?: 1) &&
            selected.size <= maximum &&
            selected.distinct().size == selected.size &&
            selected.all { id -> options.any { it.id == id } }
    }
}

@Serializable
data class QuestionAnswer(val selected: List<String> = emptyList(), val comment: String = "")

@Serializable
data class ChatQuestion(
    val id: String,
    val chatId: String,
    val runId: String = "",
    val blocking: Boolean = false,
    val fields: List<QuestionField> = emptyList(),
    val status: String = "pending",
    val createdAt: Long = 0,
)
