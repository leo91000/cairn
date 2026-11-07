package build.cairn.app.ui

import build.cairn.app.data.wireJson
import kotlinx.serialization.Serializable

@Serializable private data class AnswerLabel(val label: String)

@Serializable
private data class AnswerSummary(
    val question: String,
    val selected: List<AnswerLabel>,
    val comment: String,
)

/** Human presentation only: the original structured text still goes to the agent. */
internal fun questionAnswerText(text: String, questionId: String?): String {
    val prefix = "My answers to your questions:\n\n"
    if (questionId.isNullOrBlank() || !text.startsWith(prefix)) return text
    val fields =
        runCatching {
            wireJson.decodeFromString<List<AnswerSummary>>(text.removePrefix(prefix))
        }
            .getOrNull() ?: return text
    if (fields.size !in 1..20) return text
    return "Vos réponses\n\n" +
        fields.joinToString("\n\n") { field ->
            buildList {
                    add(field.question)
                    if (field.selected.isNotEmpty())
                        add("Choix : " + field.selected.joinToString(", ") { it.label })
                    if (field.comment.isNotEmpty())
                        add(
                            (if (field.selected.isEmpty()) "Réponse : " else "Commentaire : ") +
                                field.comment
                        )
                }
                .joinToString("\n")
        }
}
