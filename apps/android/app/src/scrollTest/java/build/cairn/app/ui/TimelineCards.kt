package build.cairn.app.ui

import androidx.compose.runtime.Composable
import build.cairn.app.data.RunEvent
import build.cairn.app.data.TimelineCard
import build.cairn.app.data.TimelineNotice
import build.cairn.contracts.TimelineContent

/** A notice card as the server sends it; device cases render it without folding events. */
internal fun noticeCard(position: Long, notice: TimelineNotice) =
    TimelineCard(
        attempt = 0,
        content = TimelineContent.C(notice),
        createdAt = position * 1000,
        id = "notice:$position",
        position = position,
        revision = position,
        runId = "run",
        source = position,
        turn = 1,
        updatedAt = position * 1000,
    )

/** The conversation's notices: the cards the server marked prominent. */
@Composable
internal fun ConversationNotices(cards: List<TimelineCard>) {
    cards
        .mapNotNull {
            (it.content as? TimelineContent.C)?.notice?.takeIf { notice -> notice.shown() }
        }
        .forEach { ChatNotice(it) }
}

/** A run-log row presented from its card. */
@Composable
internal fun RunLogRow(card: TimelineCard) {
    val notice = (card.content as TimelineContent.C).notice
    ActivityCard(
        RunEvent(id = card.source, createdAt = card.createdAt, type = "notice", text = ""),
        presentNotice(notice),
    )
}
