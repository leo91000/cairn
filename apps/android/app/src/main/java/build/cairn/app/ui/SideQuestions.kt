package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.layout
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.data.CairnViewModel
import build.cairn.app.data.Chat
import build.cairn.app.data.ChatSideQuestion
import build.cairn.app.data.Workspace
import build.cairn.contracts.AskChatSideQuestionInput
import build.cairn.contracts.ChatSideQuestionInput
import build.cairn.contracts.SideQuestionInput
import build.cairn.contracts.askChatSideQuestion
import build.cairn.contracts.cancelChatSideQuestion
import java.util.UUID

/**
 * What a reply can do with side questions: ask about it while the agent works, or show those asked
 * about it. Provided around the conversation; replies elsewhere offer neither.
 */
internal class SideQuestionActions(
    val canAsk: Boolean,
    val counts: Map<String, Int>,
    val ask: (itemId: String, quote: String) -> Unit,
    val show: (itemId: String) -> Unit,
)

internal val LocalSideQuestions = compositionLocalOf<SideQuestionActions?> { null }

/**
 * Lays out an action without adding to its line's height: its touch target overflows the line,
 * centred on it. Actions that appear and disappear then never move the history.
 */
internal fun Modifier.withoutLineHeight() = layout { measurable, constraints ->
    val placeable = measurable.measure(constraints.copy(minHeight = 0))
    layout(placeable.width, 0) { placeable.place(0, -placeable.height / 2) }
}

/** Asks about the passage selected in a reply, from its text selection menu. */
internal val LocalSideQuestionAsk = compositionLocalOf<((String) -> Unit)?> { null }

/** The side-question sheet of one conversation, and the answers its user has already read. */
@Stable
internal class SideQuestionState {
    var open by mutableStateOf(false)
    var itemId by mutableStateOf("")
    var quote by mutableStateOf("")
    var text by mutableStateOf("")
    /** Shows only the side questions about one reply. */
    var focus by mutableStateOf("")
    private var seen by mutableStateOf<Set<String>?>(null)
    private var submission = ""
    private var submissionKey = ""

    fun ask(itemId: String, quote: String) {
        this.itemId = itemId
        this.quote = quote.trim().take(4000)
        focus = ""
        open = true
    }

    /** Opens the sheet, on the side questions about one reply when [itemId] is given. */
    fun show(itemId: String = "") {
        // Questions asked from a reply's sheet are about that reply.
        if (itemId.isNotEmpty() && itemId != this.itemId) {
            this.itemId = itemId
            quote = ""
        }
        focus = itemId
        open = true
    }

    /**
     * Remembers the answers the user has seen: those already settled when the conversation opened,
     * and every answer once the sheet shows them. Call it outside composition.
     */
    fun observe(questions: List<ChatSideQuestion>) {
        val settled = questions.filter { it.status != "pending" }.map { it.id }.toSet()
        val known = seen
        if (known == null || (open && !known.containsAll(settled))) seen = known.orEmpty() + settled
    }

    /** Answers that arrived since the conversation opened, until the sheet shows them. */
    fun unread(questions: List<ChatSideQuestion>): List<ChatSideQuestion> {
        val known = seen ?: return emptyList()
        return questions.filter { it.status in setOf("answered", "failed") && it.id !in known }
    }

    /** The idempotency key of a submission, renewed only when its content changes. */
    fun submissionId(content: String): String {
        if (submissionKey != content) {
            submissionKey = content
            submission = UUID.randomUUID().toString()
        }
        return submission
    }

    /** Asked: the same question asked again later is a new one, about no reply yet. */
    fun submitted() {
        submissionKey = ""
        text = ""
        quote = ""
        itemId = ""
    }
}

private fun engine(provider: String?) =
    when (provider) {
        "claude" -> "Claude Code"
        "codex" -> "Codex"
        "opencode" -> "OpenCode"
        else -> "L’agent"
    }

/** Above the composer while a side question is answered, or its answer is unread. */
@Composable
internal fun SideQuestionCard(chat: Chat, side: SideQuestionState) {
    val pending = chat.sideQuestions.filter { it.status == "pending" }
    val unread = side.unread(chat.sideQuestions)
    val shown = pending.firstOrNull() ?: unread.firstOrNull() ?: return
    Surface(
        onClick = { side.show() },
        modifier =
            Modifier.fillMaxWidth()
                .padding(horizontal = 12.dp, vertical = 4.dp)
                .testTag("side-question-card"),
        shape = RoundedCornerShape(18.dp),
        color = MaterialTheme.colorScheme.surfaceContainerHigh,
    ) {
        Row(
            Modifier.heightIn(min = 48.dp).padding(horizontal = 16.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                CairnIcons.SideQuestion,
                null,
                Modifier.size(18.dp),
                tint = MaterialTheme.colorScheme.primary,
            )
            Spacer(Modifier.width(10.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    when {
                        pending.isNotEmpty() -> "Question à côté · ${engine(chat.provider)} répond"
                        unread.any { it.status == "answered" } -> "Question à côté · Réponse prête"
                        else -> "Question à côté · Sans réponse"
                    },
                    // Announced when the answer arrives.
                    Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                )
                Text(
                    shown.text,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
        }
    }
}

@Composable
internal fun SideQuestionSheet(
    vm: CairnViewModel,
    state: Workspace,
    chat: Chat,
    side: SideQuestionState,
    reveal: (String) -> Unit,
) {
    DetailSheet("Questions à côté", { side.open = false }) {
        SideQuestionPanel(vm, state, chat, side, reveal)
    }
}

/** The side questions of a conversation and the form to ask one, inside their sheet. */
@Composable
internal fun ColumnScope.SideQuestionPanel(
    vm: CairnViewModel,
    state: Workspace,
    chat: Chat,
    side: SideQuestionState,
    reveal: (String) -> Unit,
) {
    val shown =
        chat.sideQuestions.filter { side.focus.isEmpty() || it.itemId == side.focus }.asReversed()
    val focusManager = LocalFocusManager.current
    run {
        Text(
            "${engine(chat.provider)} répond à partir de la conversation, sans outils, pendant qu’il continue son travail. Une question à côté ne modifie ni la réponse en cours ni l’historique.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (chat.canAskSideQuestion) {
            if (side.quote.isNotEmpty())
                Surface(
                    color = MaterialTheme.colorScheme.surfaceContainerHigh,
                    shape = RoundedCornerShape(12.dp),
                ) {
                    Row(Modifier.padding(start = 12.dp), verticalAlignment = Alignment.Top) {
                        Text(
                            side.quote,
                            Modifier.weight(1f).padding(vertical = 10.dp),
                            maxLines = 6,
                            overflow = TextOverflow.Ellipsis,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        ActionIcon("Retirer le passage cité", CairnIcons.Close) { side.quote = "" }
                    }
                }
            Field(
                when {
                    side.quote.isNotEmpty() -> "Votre question sur ce passage"
                    side.itemId.isNotEmpty() -> "Votre question sur cette réponse"
                    else -> "Votre question"
                },
                side.text,
                { side.text = it.take(4000) },
                3,
                enabled = !state.busy,
            )
            Button(
                onClick = {
                    val question = side.text.trim()
                    val content =
                        listOf(chat.id, question, side.quote, side.itemId).joinToString("\u0000")
                    val id = side.submissionId(content)
                    // Close the keyboard so the answer appears in view.
                    focusManager.clearFocus()
                    vm.perform {
                        api.askChatSideQuestion(
                            AskChatSideQuestionInput(
                                chatId = chat.id,
                                question =
                                    SideQuestionInput(
                                        id = id,
                                        text = question,
                                        quote = side.quote,
                                        itemId = side.itemId,
                                    ),
                            )
                        )
                        side.submitted()
                    }
                },
                enabled = !state.busy && side.text.isNotBlank(),
                modifier = Modifier.align(Alignment.End),
            ) {
                Text("Demander")
            }
        } else
            Notice(
                // The same cases as the server's `sideQuestionReason`, in French.
                when {
                    chat.status != "running" ->
                        "Les questions à côté sont possibles pendant que Claude Code travaille sur une réponse. Envoyez plutôt un message."
                    chat.provider == "codex" ->
                        "Codex ne peut pas répondre aux questions à côté dans Cairn : ses conversations à côté peuvent encore utiliser des outils. Posez-la dans la conversation."
                    chat.provider == "opencode" ->
                        "OpenCode ne peut pas encore répondre aux questions à côté dans Cairn. Posez-la dans la conversation."
                    chat.run?.cancelRequestedAt != null ->
                        "La réponse s’arrête. Posez-la dans la conversation quand elle reprend."
                    chat.compaction?.running == true ->
                        "La conversation compacte son contexte. Réessayez une fois la compaction terminée."
                    else -> "Claude Code démarre encore sa réponse. Réessayez dans un instant."
                },
                icon = CairnIcons.SideQuestion,
                maxLines = 4,
            )
        state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        if (side.focus.isNotEmpty())
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "À propos d’une réponse",
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                TextButton(onClick = { side.focus = "" }) { Text("Tout afficher") }
            }
        if (shown.isEmpty())
            Text(
                "Aucune question à côté pour l’instant.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        shown.forEach { question ->
            SideQuestionRow(vm, state, question) {
                side.open = false
                reveal(question.itemId)
            }
        }
    }
}

@Composable
private fun SideQuestionRow(
    vm: CairnViewModel,
    state: Workspace,
    question: ChatSideQuestion,
    reveal: () -> Unit,
) {
    Surface(
        Modifier.fillMaxWidth().testTag("side-question"),
        shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.surfaceContainerLow,
    ) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            if (question.quote.isNotEmpty())
                Text(
                    question.quote,
                    maxLines = 3,
                    overflow = TextOverflow.Ellipsis,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            Text(
                question.text,
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.Medium,
            )
            // The answer or failure is announced when it replaces « Réponse en cours… ».
            Box(Modifier.semantics { liveRegion = LiveRegionMode.Polite }) {
                when (question.status) {
                    "pending" ->
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Box(
                                Modifier.size(6.dp)
                                    .clip(CircleShape)
                                    .background(MaterialTheme.colorScheme.primary)
                            )
                            Spacer(Modifier.width(8.dp))
                            Text(
                                "Réponse en cours…",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    "answered" ->
                        Box(Modifier.testTag("side-question-answer")) {
                            Markdown(question.answer.orEmpty())
                        }
                    "failed" ->
                        Text(
                            question.error.orEmpty(),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                        )
                    else ->
                        Text(
                            "Retirée",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                }
            }
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                if (question.itemId.isNotEmpty())
                    TextButton(onClick = reveal) { Text("Voir la réponse") }
                else Spacer(Modifier)
                if (question.status == "pending")
                    TextButton(
                        onClick = {
                            vm.perform {
                                api.cancelChatSideQuestion(
                                    ChatSideQuestionInput(
                                        chatId = question.chatId,
                                        sideQuestionId = question.id,
                                    )
                                )
                            }
                        },
                        enabled = !state.busy,
                    ) {
                        Text("Retirer")
                    }
            }
        }
    }
}
