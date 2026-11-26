@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
    androidx.compose.foundation.ExperimentalFoundationApi::class,
)

package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.SecureFlagPolicy
import build.cairn.app.data.*
import build.cairn.contracts.AnswerChatQuestionInput
import build.cairn.contracts.ChatQuestionInput
import build.cairn.contracts.QuestionAnswerInput
import build.cairn.contracts.answerChatQuestion
import build.cairn.contracts.cancelChatQuestion
import java.util.UUID
import kotlinx.serialization.json.*

@Composable
internal fun QuestionDialog(
    vm: CairnViewModel,
    state: Workspace,
    question: ChatQuestion,
    questions: List<ChatQuestion>,
    select: (ChatQuestion) -> Unit,
    close: () -> Unit,
) {
    val supported = question.fields.isNotEmpty() && question.fields.all { it.supportsAnswers }
    // Private answers never enter SavedState or on-disk drafts.
    var answers by remember(question.id) { mutableStateOf<Map<String, String>>(emptyMap()) }
    var selected by remember(question.id) { mutableStateOf<Map<String, List<String>>>(emptyMap()) }
    var submission by remember(question.id) { mutableStateOf("") }
    var submissionKey by remember(question.id) { mutableStateOf("") }
    Dialog(
        onDismissRequest = { if (!state.busy) close() },
        properties =
            DialogProperties(
                usePlatformDefaultWidth = false,
                securePolicy =
                    if (question.fields.any { it.secret }) SecureFlagPolicy.SecureOn
                    else SecureFlagPolicy.Inherit,
            ),
    ) {
        Scaffold(
            modifier = Modifier.imePadding(),
            topBar = { TopAppBar(title = { Text("Votre réponse") }) },
        ) { padding ->
            Column(Modifier.padding(padding).fillMaxSize()) {
                ConnectionFeedbackNotice()
                Box(Modifier.weight(1f)) {
                    Page {
                        Text(
                            if (question.blocking) "L’agent attend votre réponse."
                            else "Répondez quand vous êtes prêt."
                        )
                        if (questions.size > 1)
                            Choice(
                                "Question",
                                question.id,
                                questions.map {
                                    it.id to it.fields.firstOrNull()?.title.orEmpty().take(100)
                                },
                            ) { key ->
                                select(questions.first { it.id == key })
                            }
                        if (!supported)
                            Text(
                                "Cette question utilise un format non pris en charge. Mettez à jour l’installation Cairn, puis demandez à l’agent de reposer la question. Vous pouvez retirer cette demande ci-dessous.",
                                color = MaterialTheme.colorScheme.error,
                            )
                        if (supported)
                            question.fields.forEach { field ->
                                Text(field.title, style = MaterialTheme.typography.titleMedium)
                                field.options.forEach { option ->
                                    val checked = selected[field.id].orEmpty().contains(option.id)
                                    val toggle = {
                                        val current = selected[field.id].orEmpty()
                                        val next =
                                            if (field.mode == "single") listOf(option.id)
                                            else if (checked) current - option.id
                                            else current + option.id
                                        selected = selected + (field.id to next)
                                    }
                                    OutlinedCard(
                                        onClick = toggle,
                                        modifier = Modifier.fillMaxWidth(),
                                        colors =
                                            CardDefaults.outlinedCardColors(
                                                containerColor =
                                                    if (checked)
                                                        MaterialTheme.colorScheme.secondaryContainer
                                                    else MaterialTheme.colorScheme.surface
                                            ),
                                    ) {
                                        Row(
                                            Modifier.padding(12.dp),
                                            verticalAlignment = Alignment.CenterVertically,
                                        ) {
                                            if (field.mode == "multiple")
                                                Checkbox(checked, { toggle() })
                                            else RadioButton(checked, toggle)
                                            Column {
                                                Text(option.label)
                                                if (option.recommended)
                                                    Text(
                                                        "Recommandé",
                                                        color = MaterialTheme.colorScheme.primary,
                                                    )
                                                if (option.description.isNotBlank())
                                                    Text(
                                                        option.description,
                                                        style = MaterialTheme.typography.bodySmall,
                                                    )
                                            }
                                        }
                                    }
                                }
                                if (field.options.isNotEmpty()) {
                                    val minimum = field.minChoices ?: 1
                                    val maximum =
                                        if (field.mode == "single") 1
                                        else field.maxChoices ?: field.options.size
                                    val range =
                                        if (minimum == maximum) "$minimum"
                                        else "$minimum à $maximum"
                                    Text(
                                        if (field.mode == "multiple")
                                            "Choisissez $range options, ou répondez en texte libre."
                                        else "Choisissez une option, ou répondez en texte libre.",
                                        style = MaterialTheme.typography.bodySmall,
                                    )
                                    if (field.mode == "multiple") {
                                        val count = selected[field.id].orEmpty().size
                                        val detail =
                                            when {
                                                count > maximum ->
                                                    " · Retirez ${count - maximum} choix"
                                                count in 1 until minimum ->
                                                    " · Encore ${minimum - count} choix"
                                                else -> ""
                                            }
                                        Text(
                                            "Choix sélectionnés : $count$detail",
                                            style = MaterialTheme.typography.bodySmall,
                                            color =
                                                if (detail.isNotEmpty())
                                                    MaterialTheme.colorScheme.error
                                                else MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                    }
                                    if (selected[field.id].orEmpty().isNotEmpty())
                                        TextButton(
                                            onClick = {
                                                selected = selected + (field.id to emptyList())
                                            }
                                        ) {
                                            Text("Effacer les choix")
                                        }
                                }
                                if (field.secret)
                                    OutlinedTextField(
                                        answers[field.id].orEmpty(),
                                        { answers = answers + (field.id to it.take(10000)) },
                                        Modifier.fillMaxWidth(),
                                        label = { Text("Réponse privée") },
                                        visualTransformation = PasswordVisualTransformation(),
                                        keyboardOptions = InputKeyboards.Password,
                                    )
                                else
                                    Field(
                                        if (field.options.isEmpty()) "Réponse"
                                        else "Commentaire ou réponse libre",
                                        answers[field.id].orEmpty(),
                                        { answers = answers + (field.id to it.take(10000)) },
                                        2,
                                    )
                            }
                        state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                        TextButton(
                            onClick = {
                                vm.perform {
                                    api.cancelChatQuestion(
                                        ChatQuestionInput(
                                            chatId = question.chatId,
                                            questionId = question.id,
                                        )
                                    )
                                    answers = emptyMap()
                                    selected = emptyMap()
                                    close()
                                }
                            },
                            enabled = !state.busy,
                        ) {
                            Text("Ce n’est plus utile")
                        }
                    }
                }
                Row(
                    Modifier.fillMaxWidth().padding(16.dp),
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    TextButton(onClick = close, enabled = !state.busy) { Text("Plus tard") }
                    Button(
                        onClick = {
                            vm.perform {
                                val values =
                                    wireJson.encodeToJsonElement(
                                        question.fields.associate { field ->
                                            field.id to
                                                QuestionAnswer(
                                                    selected =
                                                        field.options
                                                            .filter {
                                                                selected[field.id]
                                                                    .orEmpty()
                                                                    .contains(it.id)
                                                            }
                                                            .map { it.id },
                                                    comment = answers[field.id].orEmpty().trim(),
                                                )
                                        }
                                    )
                                if (submissionKey != values.toString()) {
                                    submissionKey = values.toString()
                                    submission = UUID.randomUUID().toString()
                                }
                                api.answerChatQuestion(
                                    AnswerChatQuestionInput(
                                        chatId = question.chatId,
                                        questionId = question.id,
                                        answer =
                                            QuestionAnswerInput(
                                                id = submission,
                                                answers = wireJson.decodeFromJsonElement(values),
                                            ),
                                    )
                                )
                                answers = emptyMap()
                                selected = emptyMap()
                                close()
                            }
                        },
                        enabled =
                            !state.busy &&
                                supported &&
                                question.fields.all {
                                    it.accepts(selected[it.id].orEmpty(), answers[it.id].orEmpty())
                                },
                    ) {
                        Text("Envoyer la réponse")
                    }
                }
            }
        }
    }
}

/** Who and where, chosen directly on screen instead of through drop-down menus. */
