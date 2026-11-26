package build.cairn.app.ui

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.onPlaced
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import build.cairn.app.data.CairnViewModel
import build.cairn.app.data.Chat
import build.cairn.app.data.ChatSideQuestion
import build.cairn.app.data.TimelineCard
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
    val ask: (itemId: String, quote: String, replyEventId: Long?) -> Unit,
    val show: (itemId: String, replyEventId: Long?, lastEventId: Long?) -> Unit,
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

internal data class SideQuestionDraft(
    val text: String,
    val quote: String,
    val itemId: String,
    val replyEventId: Long?,
    val revision: Long,
)

/** At most [count] code points, as the server counts them, never cutting an emoji. */
internal fun String.takeCodePoints(count: Int): String =
    substring(0, offsetByCodePoints(0, minOf(count, codePointCount(0, length))))

/** The side-question sheet of one conversation, and the answers its user has already read. */
@Stable
internal class SideQuestionState {
    var open by mutableStateOf(false)
    var itemId by mutableStateOf("")
    var replyEventId by mutableStateOf<Long?>(null)
    var quote by mutableStateOf("")
    var text by mutableStateOf("")
    /** Shows only the side questions about one reply. */
    var focus by mutableStateOf("")
    var focusEventId by mutableStateOf<Long?>(null)
    var focusLastEventId by mutableStateOf<Long?>(null)
    /** Why « Voir la réponse » could not show the reply. */
    var notice by mutableStateOf("")
    private var seen by mutableStateOf<Set<String>?>(null)
    private var submission = ""
    private var submissionKey = ""
    private var revision = 0L

    fun draft() = SideQuestionDraft(text, quote, itemId, replyEventId, revision)

    fun editText(value: String) {
        if (text != value) revision++
        text = value
    }

    fun clearQuote() {
        if (quote.isNotEmpty()) revision++
        quote = ""
    }

    fun showAll() {
        if (focus.isNotEmpty()) revision++
        focus = ""
    }

    fun ask(itemId: String, quote: String, replyEventId: Long? = null) {
        revision++
        this.itemId = itemId
        this.replyEventId = replyEventId
        this.quote = quote.trim().takeCodePoints(4000)
        focus = ""
        focusEventId = null
        focusLastEventId = null
        notice = ""
        open = true
    }

    /** Opens the sheet, on the side questions about one reply when [itemId] is given. */
    fun show(itemId: String = "", replyEventId: Long? = null, lastEventId: Long? = replyEventId) {
        revision++
        // Questions asked from a reply's sheet are about that reply.
        if (itemId.isNotEmpty() && (itemId != this.itemId || replyEventId != this.replyEventId)) {
            this.itemId = itemId
            quote = ""
        }
        if (itemId.isNotEmpty()) this.replyEventId = replyEventId
        focus = itemId
        focusEventId = replyEventId
        focusLastEventId = lastEventId
        notice = ""
        open = true
    }

    /** Reopens the sheet when the reply a question is about is not loaded. */
    fun unreachable(itemId: String, replyEventId: Long?) {
        show(itemId, replyEventId)
        notice =
            "Cette réponse n’est pas dans la conversation chargée. Remontez pour charger les messages précédents, puis réessayez."
    }

    /** Keeps the focused reply's range current as streaming or older pages extend it. */
    fun follow(cards: List<TimelineCard>) {
        if (focus.isEmpty()) return
        val anchor = focusEventId ?: return
        val card =
            cards.firstOrNull {
                (it.content as? build.cairn.contracts.TimelineContent.B)?.itemId == focus &&
                    anchor in it.position..it.source
            } ?: return
        focusEventId = card.position
        focusLastEventId = card.source
    }

    /**
     * Remembers the answers the user has seen: those already settled when the conversation opened,
     * and every answer once the sheet shows them. Call it outside composition.
     */
    fun observe(questions: List<ChatSideQuestion>) {
        val settled =
            questions
                .filter { it.status != "pending" && (seen == null || shows(it)) }
                .map { it.id }
                .toSet()
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

    /** Asked: the next question remains about the focused reply, if any. */
    fun submitted(sent: SideQuestionDraft, id: String) {
        if (submission == id) submissionKey = ""
        if (draft() != sent) return
        text = ""
        quote = ""
        itemId = focus
        if (focus.isEmpty()) replyEventId = null
    }

    fun shows(question: ChatSideQuestion): Boolean {
        if (focus.isEmpty()) return true
        if (question.itemId != focus) return false
        if (question.replyEventId.isEmpty()) return true
        val id = question.replyEventId.toLongOrNull()
        val first = focusEventId
        val last = focusLastEventId
        return id != null && first != null && last != null && id in first..last
    }
}

private fun engine(provider: String?) =
    when (provider) {
        "claude" -> "Claude Code"
        "codex" -> "Codex"
        "opencode" -> "OpenCode"
        "antigravity" -> "Antigravity"
        else -> "L’agent"
    }

internal fun hasSideQuestionAction(chat: Chat?): Boolean =
    chat != null && (chat.canAskSideQuestion || chat.sideQuestions.isNotEmpty())

internal enum class SideQuestionPlacement {
    Working,
    Waiting,
    Header,
    FullscreenHeader,
}

/** Keep keyboard focus when the visible side-question button moves between placements. */
@Composable
internal fun rememberSideQuestionButtonFocus(
    placement: SideQuestionPlacement?,
    fallback: FocusRequester,
): Modifier {
    val requester = remember { FocusRequester() }
    var focused by remember { mutableStateOf(false) }
    var placed by remember { mutableStateOf<SideQuestionPlacement?>(null) }
    // Take the snapshot before Compose detaches the outgoing focus target.
    val restoreFocus = focused
    LaunchedEffect(placement) {
        if (restoreFocus) {
            withFrameNanos {}
            // Keep the reading position when the new working row is outside the visible history.
            val restored =
                placement != null &&
                    placed == placement &&
                    runCatching { requester.requestFocus() }.getOrDefault(false)
            if (!restored) runCatching { fallback.requestFocus() }
        }
    }
    return Modifier.focusRequester(requester)
        .onFocusChanged { focused = it.isFocused }
        .onPlaced { placed = placement }
}

internal class SideQuestionStatus(chat: Chat, side: SideQuestionState) {
    val pending = chat.sideQuestions.any { it.status == "pending" }
    private val unread = side.unread(chat.sideQuestions)
    val answers = unread.count { it.status == "answered" }
    val failures = unread.count { it.status == "failed" }
    val announcement =
        listOfNotNull(
                if (pending) "${engine(chat.provider)} répond" else null,
                if (answers > 0) "Réponse prête ($answers)" else null,
                if (failures > 0) "Sans réponse ($failures)" else null,
            )
            .joinToString(" · ")
            .takeIf { it.isNotEmpty() }
            ?.let { "Question à côté · $it" }
}

/** Keep announcements available while the working row is scrolled out of the history. */
@Composable
internal fun SideQuestionAnnouncement(status: SideQuestionStatus?) {
    Box(
        Modifier.size(1.dp).testTag("side-question-status").semantics {
            contentDescription = status?.announcement.orEmpty()
            liveRegion = LiveRegionMode.Polite
        }
    )
}

/**
 * Compact icon-only trigger for side questions: lives on the working row while the agent works and
 * moves to the header when saved or unread answers remain after the turn.
 */
@Composable
internal fun SideQuestionButton(
    chat: Chat,
    side: SideQuestionState,
    modifier: Modifier = Modifier,
    size: Dp = 36.dp,
    status: SideQuestionStatus = SideQuestionStatus(chat, side),
) {
    val hasQuestions = chat.sideQuestions.isNotEmpty()
    if (!hasQuestions && !chat.canAskSideQuestion) return

    val isPending = status.pending
    val answers = status.answers
    val hasUnreadAnswer = answers > 0
    val hasUnreadFailure = !hasUnreadAnswer && status.failures > 0
    val label =
        status.announcement
            ?: if (hasQuestions) "Questions à côté · Enregistrées (${chat.sideQuestions.size})"
            else "Poser une question à côté"

    val colors = MaterialTheme.colorScheme
    val container =
        when {
            isPending -> colors.primaryContainer.copy(alpha = 0.65f)
            hasUnreadAnswer -> colors.primaryContainer
            hasUnreadFailure -> colors.errorContainer
            else -> colors.surface
        }
    val iconTint =
        when {
            isPending -> colors.primary
            hasUnreadAnswer -> colors.onPrimaryContainer
            hasUnreadFailure -> colors.error
            hasQuestions -> colors.onSurface
            else -> colors.onSurfaceVariant
        }
    val borderColor =
        when {
            isPending -> colors.primary.copy(alpha = 0.28f)
            hasUnreadAnswer -> colors.primary.copy(alpha = 0.65f)
            hasUnreadFailure -> colors.error.copy(alpha = 0.55f)
            else -> colors.outlineVariant
        }
    val borderWidth = if (!isPending && (hasUnreadAnswer || hasUnreadFailure)) 1.5.dp else 1.dp

    val badgeText =
        when {
            hasUnreadAnswer -> if (answers > 99) "99+" else answers.toString()
            hasUnreadFailure -> "!"
            !isPending && hasQuestions ->
                if (chat.sideQuestions.size > 99) "99+" else chat.sideQuestions.size.toString()
            else -> null
        }
    val badgeBg =
        when {
            hasUnreadAnswer -> colors.primary
            hasUnreadFailure -> colors.error
            else -> colors.surfaceContainerHighest
        }
    val badgeFg =
        when {
            hasUnreadAnswer -> colors.onPrimary
            hasUnreadFailure -> colors.onError
            else -> colors.onSurface
        }

    Box(
        modifier =
            modifier
                .size(maxOf(size, 48.dp))
                .testTag("side-question-button")
                .semantics { contentDescription = label }
                .clickable(role = Role.Button) {
                    if (!hasQuestions) side.ask("", "") else side.show()
                },
        contentAlignment = Alignment.Center,
    ) {
        Box(Modifier.size(size + 8.dp), contentAlignment = Alignment.Center) {
            Box(
                Modifier.size(size)
                    .clip(CircleShape)
                    .background(container)
                    .border(borderWidth, borderColor, CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    CairnIcons.SideQuestion,
                    null,
                    Modifier.size(size * 0.47f),
                    tint = iconTint,
                )
            }
            if (isPending) {
                val transition = rememberInfiniteTransition(label = "side-question-pending")
                val angle by
                    transition.animateFloat(
                        0f,
                        360f,
                        infiniteRepeatable(tween(1200, easing = LinearEasing)),
                        label = "spin",
                    )
                val arcColor = colors.primary
                Canvas(Modifier.size(size)) {
                    val stroke = 2.dp.toPx()
                    drawArc(
                        color = arcColor,
                        startAngle = angle - 90f,
                        sweepAngle = 95f,
                        useCenter = false,
                        style = Stroke(width = stroke, cap = StrokeCap.Round),
                    )
                }
            }
            if (badgeText != null)
                Box(
                    Modifier.align(Alignment.TopEnd)
                        .defaultMinSize(minWidth = 17.dp, minHeight = 17.dp)
                        .clip(CircleShape)
                        .background(badgeBg)
                        .border(1.5.dp, colors.background, CircleShape)
                        .padding(horizontal = 4.dp)
                        .clearAndSetSemantics {},
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        badgeText,
                        style = MaterialTheme.typography.labelSmall,
                        fontWeight = FontWeight.Bold,
                        color = badgeFg,
                        maxLines = 1,
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
    reveal: (String, Long?) -> Unit,
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
    reveal: (String, Long?) -> Unit,
) {
    val shown = chat.sideQuestions.filter(side::shows).asReversed()
    val focusManager = LocalFocusManager.current
    run {
        Text(
            "${engine(chat.provider)} répond à partir de la conversation en refusant les outils demandés, pendant qu’il continue son travail. Une question à côté ne modifie ni la réponse en cours ni l’historique.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (chat.provider == "claude")
            Text(
                "Claude refuse les outils demandés, mais les hooks de politique gérée par l’opérateur peuvent s’exécuter avant ce refus et produire des effets. Retirer une question ne peut pas annuler les effets déjà produits.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        else if (chat.provider == "antigravity")
            Text(
                "Antigravity répond à partir de la conversation enregistrée jusqu’à sa dernière étape terminée, pas de l’étape en cours.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        if (chat.canAskSideQuestion) {
            if (side.itemId.isNotEmpty())
                TextButton(onClick = { side.ask("", "") }) { Text("Poser une question générale") }
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
                        ActionIcon("Retirer le passage cité", CairnIcons.Close) {
                            side.clearQuote()
                        }
                    }
                }
            Field(
                when {
                    side.quote.isNotEmpty() -> "Votre question sur ce passage"
                    side.itemId.isNotEmpty() -> "Votre question sur cette réponse"
                    else -> "Votre question"
                },
                side.text,
                { side.editText(it.takeCodePoints(4000)) },
                3,
                enabled = !state.busy,
            )
            Button(
                onClick = {
                    val draft = side.draft()
                    val question = draft.text.trim()
                    val content =
                        listOf(chat.id, question, draft.quote, draft.itemId, draft.replyEventId)
                            .joinToString("\u0000")
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
                                        quote = draft.quote,
                                        itemId = draft.itemId,
                                        replyEventId = draft.replyEventId?.toString().orEmpty(),
                                    ),
                            )
                        )
                        side.submitted(draft, id)
                    }
                },
                enabled = !state.busy && side.text.isNotBlank(),
                modifier = Modifier.align(Alignment.End),
            ) {
                Text("Demander")
            }
        } else
            Notice(
                // The server's reason, worded in French.
                when (chat.sideQuestionUnavailable) {
                    "between-replies" ->
                        "Les questions à côté sont possibles pendant que l’agent travaille sur une réponse. Envoyez plutôt un message."
                    "codex" ->
                        "Codex ne peut pas répondre aux questions à côté dans Cairn : ses conversations à côté peuvent encore utiliser des outils. Posez-la dans la conversation."
                    "opencode" ->
                        "OpenCode ne peut pas encore répondre aux questions à côté dans Cairn. Posez-la dans la conversation."
                    "unknown-agent" ->
                        "Cet agent ne peut pas répondre aux questions à côté dans Cairn. Posez-la dans la conversation."
                    "stopping" ->
                        "La réponse s’arrête. Posez-la dans la conversation quand elle reprend."
                    "compacting" ->
                        "La conversation compacte son contexte. Réessayez une fois la compaction terminée."
                    "claude-release" ->
                        "Cette version de Claude Code ne peut pas répondre aux questions à côté dans Cairn : seule la version 2.1.308 a été qualifiée. Posez-la dans la conversation."
                    "not-offered" ->
                        "${if (chat.provider == "antigravity") "Antigravity" else "Claude Code"} n’a pas encore proposé de questions à côté pour cette réponse. Réessayez dans un instant ; s’il ne le fait jamais, son runtime doit peut-être être mis à jour."
                    // A reason this version does not know yet: the server's own words.
                    else ->
                        chat.sideQuestionReason
                            ?: "Les questions à côté ne sont pas disponibles pour cette réponse."
                },
                icon = CairnIcons.SideQuestion,
                maxLines = 4,
            )
        state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        if (side.notice.isNotEmpty()) Text(side.notice, color = MaterialTheme.colorScheme.error)
        if (side.focus.isNotEmpty())
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "À propos d’une réponse",
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                TextButton(onClick = { side.showAll() }) { Text("Tout afficher") }
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
                reveal(question.itemId, question.replyEventId.toLongOrNull())
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
            // One persistent status per question, announced when the answer arrives: the
            // Markdown answer itself renders outside the Compose semantics tree.
            Row(
                Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (question.status == "pending") {
                    Box(
                        Modifier.size(6.dp)
                            .clip(CircleShape)
                            .background(MaterialTheme.colorScheme.primary)
                    )
                    Spacer(Modifier.width(8.dp))
                }
                Text(
                    when (question.status) {
                        "pending" -> "Réponse en cours…"
                        "answered" -> "Répondue"
                        "failed" -> "Sans réponse"
                        else -> "Retirée"
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            when (question.status) {
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
