package build.cairn.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.PathBuilder
import androidx.compose.ui.graphics.vector.path
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

/*
 * Shared building blocks of the "Signal" design: agent identities, section labels, pills,
 * round buttons and the floating navigation dock.
 */

// Distinct, readable on white text in both themes; assigned deterministically per agent/project.
private val IdentityColors =
    listOf(
        Color(0xFF4545EF),
        Color(0xFFD9542F),
        Color(0xFFB23F8C),
        Color(0xFF12806F),
        Color(0xFF8A5A12),
        Color(0xFF3F7FBF),
        Color(0xFF7A4FD1),
        Color(0xFF5E6B2E),
    )

internal fun identityColor(key: String): Color =
    IdentityColors[Math.floorMod(key.hashCode(), IdentityColors.size)]

internal fun initial(name: String): String =
    name.trim().firstOrNull { it.isLetterOrDigit() }?.uppercaseChar()?.toString() ?: "?"

internal enum class AvatarBadge {
    LIVE,
    ATTENTION,
}

@Composable
internal fun AgentAvatar(
    name: String,
    key: String = name,
    size: Dp = 44.dp,
    badge: AvatarBadge? = null,
) {
    Box(Modifier.clearAndSetSemantics {}) {
        Box(
            Modifier.size(size)
                .clip(RoundedCornerShape(size * 0.3f))
                .background(identityColor(key)),
            contentAlignment = Alignment.Center,
        ) {
            AgentPortraitImage(key) {
                Text(
                    initial(name),
                    style = MaterialTheme.typography.titleMedium,
                    fontSize = (size.value * 0.42f).sp,
                    fontWeight = FontWeight.Bold,
                    color = Color.White,
                )
            }
        }
        if (badge != null)
            Box(
                Modifier.align(Alignment.BottomEnd)
                    .offset(3.dp, 3.dp)
                    .size(15.dp)
                    .clip(CircleShape)
                    .background(MaterialTheme.colorScheme.background)
                    .padding(3.dp)
                    .clip(CircleShape)
                    .background(
                        if (badge == AvatarBadge.LIVE) MaterialTheme.colorScheme.primary
                        else signal.attention
                    )
            )
    }
}

@Composable
internal fun ProjectLabel(name: String, key: String?, modifier: Modifier = Modifier) {
    Row(modifier, verticalAlignment = Alignment.CenterVertically) {
        // No project means "every allowed project": a neutral outline rather than an identity
        // colour.
        Box(
            Modifier.size(7.dp)
                .then(
                    if (key != null) Modifier.background(identityColor(key), CircleShape)
                    else
                        Modifier.border(
                            1.dp,
                            MaterialTheme.colorScheme.onSurfaceVariant,
                            CircleShape,
                        )
                )
        )
        Spacer(Modifier.width(6.dp))
        Text(
            name,
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
internal fun Eyebrow(text: String, modifier: Modifier = Modifier) {
    Text(
        text.uppercase(Locale.FRENCH),
        modifier,
        style = MaterialTheme.typography.labelSmall,
        fontWeight = FontWeight.Bold,
        letterSpacing = 1.2.sp,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

/** Large bold screen title with an optional one-line summary. */
@Composable
internal fun ScreenTitle(
    title: String,
    summary: String = "",
    modifier: Modifier = Modifier,
    action: @Composable () -> Unit = {},
) {
    Row(modifier.fillMaxWidth(), verticalAlignment = Alignment.Bottom) {
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.headlineLarge)
            if (summary.isNotBlank())
                Text(
                    summary,
                    Modifier.padding(top = 4.dp),
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
        }
        action()
    }
}

/** A circular action with a 48 dp touch target. */
@Composable
internal fun RoundAction(
    label: String,
    icon: ImageVector,
    modifier: Modifier = Modifier,
    container: Color = MaterialTheme.colorScheme.surface,
    content: Color = MaterialTheme.colorScheme.onSurface,
    outlined: Boolean = container == MaterialTheme.colorScheme.surface,
    size: Dp = 44.dp,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    Surface(
        onClick = onClick,
        enabled = enabled,
        modifier = modifier.size(maxOf(size, 48.dp)).semantics { contentDescription = label },
        color = Color.Transparent,
        shape = CircleShape,
    ) {
        Box(contentAlignment = Alignment.Center) {
            Box(
                Modifier.size(size)
                    .clip(CircleShape)
                    .background(if (enabled) container else container.copy(alpha = 0.5f))
                    .then(
                        if (outlined)
                            Modifier.border(
                                1.dp,
                                MaterialTheme.colorScheme.outlineVariant,
                                CircleShape,
                            )
                        else Modifier
                    ),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    icon,
                    null,
                    Modifier.size(size * 0.45f),
                    tint = if (enabled) content else content.copy(alpha = 0.4f),
                )
            }
        }
    }
}

/** Rounded, filled text action. */
@Composable
internal fun SignalButton(
    text: String,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
    container: Color = signal.ink,
    content: Color = signal.onInk,
    enabled: Boolean = true,
    border: Boolean = false,
    height: Dp = 44.dp,
    /** Fill the width given by [modifier] instead of wrapping the label. */
    expand: Boolean = false,
    onClick: () -> Unit,
) {
    Surface(
        onClick = onClick,
        enabled = enabled,
        modifier = modifier.heightIn(min = maxOf(height, 48.dp)),
        color = Color.Transparent,
        shape = CircleShape,
    ) {
        Box(contentAlignment = Alignment.Center) {
            Row(
                Modifier.height(height)
                    .then(if (expand) Modifier.fillMaxWidth() else Modifier)
                    .clip(CircleShape)
                    .background(if (enabled) container else container.copy(alpha = 0.4f))
                    .then(
                        if (border)
                            Modifier.border(
                                1.dp,
                                MaterialTheme.colorScheme.outlineVariant,
                                CircleShape,
                            )
                        else Modifier
                    )
                    .padding(horizontal = 16.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp, Alignment.CenterHorizontally),
            ) {
                if (icon != null) Icon(icon, null, Modifier.size(17.dp), tint = content)
                Text(
                    text,
                    style = MaterialTheme.typography.labelLarge,
                    fontWeight = FontWeight.SemiBold,
                    color = content,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

/** Selectable filter chip: ink when selected, outlined otherwise. */
@Composable
internal fun SignalChip(
    text: String,
    selected: Boolean,
    count: Int? = null,
    leading: Color? = null,
    onClick: () -> Unit,
) {
    val ink = signal.ink
    Row(
        Modifier.heightIn(min = 48.dp).selectable(selected, role = Role.Tab, onClick = onClick),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Row(
            Modifier.height(36.dp)
                .clip(CircleShape)
                .background(if (selected) ink else Color.Transparent)
                .border(
                    1.dp,
                    if (selected) ink else MaterialTheme.colorScheme.outlineVariant,
                    CircleShape,
                )
                .padding(horizontal = 14.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(7.dp),
        ) {
            if (leading != null) Box(Modifier.size(8.dp).background(leading, CircleShape))
            Text(
                text,
                style = MaterialTheme.typography.labelLarge,
                fontWeight = FontWeight.SemiBold,
                color = if (selected) signal.onInk else MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
            )
            if (count != null)
                Text(
                    count.toString(),
                    style = MaterialTheme.typography.labelLarge,
                    color =
                        if (selected) signal.onInk.copy(alpha = 0.6f)
                        else MaterialTheme.colorScheme.onSurfaceVariant,
                )
        }
    }
}

/** Card surface used by the redesigned screens. */
@Composable
internal fun SignalCard(
    modifier: Modifier = Modifier,
    onClick: (() -> Unit)? = null,
    color: Color = MaterialTheme.colorScheme.surface,
    outlined: Boolean = MaterialTheme.colorScheme.background.luminance() < 0.2f,
    padding: PaddingValues = PaddingValues(16.dp),
    content: @Composable ColumnScope.() -> Unit,
) {
    val shape = RoundedCornerShape(22.dp)
    val border =
        if (outlined) BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant) else null
    if (onClick != null)
        Surface(
            onClick = onClick,
            modifier = modifier,
            shape = shape,
            color = color,
            border = border,
        ) {
            Column(Modifier.padding(padding), content = content)
        }
    else
        Surface(modifier = modifier, shape = shape, color = color, border = border) {
            Column(Modifier.padding(padding), content = content)
        }
}

/** Compact live status: "Cairn travaille · 14 min". */
@Composable
internal fun LiveChip(text: String, modifier: Modifier = Modifier) {
    Row(
        modifier
            .clip(CircleShape)
            .background(MaterialTheme.colorScheme.primary)
            .padding(horizontal = 8.dp, vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(6.dp).background(MaterialTheme.colorScheme.onPrimary, CircleShape))
        Spacer(Modifier.width(6.dp))
        Text(
            text,
            style = MaterialTheme.typography.labelMedium,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.onPrimary,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** Minutes/hours since [start], e.g. "14 min" or "2 h 05". */
internal fun elapsed(start: Long?, now: Long = System.currentTimeMillis()): String {
    if (start == null || start <= 0) return ""
    val minutes = ((now - start).coerceAtLeast(0) / 60000)
    return if (minutes < 1) "< 1 min"
    else if (minutes < 60) "$minutes min"
    else "${minutes / 60} h ${(minutes % 60).toString().padStart(2, '0')}"
}

/** Short relative stamp for lists: time today, "Hier", weekday this week, else a date. */
internal fun shortStamp(
    value: Long,
    now: Long = System.currentTimeMillis(),
    zone: ZoneId = ZoneId.systemDefault(),
): String {
    if (value <= 0) return ""
    val day = Instant.ofEpochMilli(value).atZone(zone)
    val today = Instant.ofEpochMilli(now).atZone(zone).toLocalDate()
    val date: LocalDate = day.toLocalDate()
    return when {
        date == today -> DateTimeFormatter.ofPattern("HH:mm").format(day)
        date == today.minusDays(1) -> "Hier"
        date.isAfter(today.minusDays(7)) ->
            DateTimeFormatter.ofPattern("EEE", Locale.FRENCH).format(day).replaceFirstChar {
                it.titlecase(Locale.FRENCH)
            }
        date.year == today.year -> DateTimeFormatter.ofPattern("d MMM", Locale.FRENCH).format(day)
        else -> DateTimeFormatter.ofPattern("d MMM yyyy", Locale.FRENCH).format(day)
    }
}

/**
 * Upcoming date for schedules: "Aujourd’hui · 09:00", "Demain · 09:00", "lun. 29 sept. · 09:00".
 */
internal fun upcomingStamp(
    value: Long,
    now: Long = System.currentTimeMillis(),
    zone: ZoneId = ZoneId.systemDefault(),
): String {
    if (value <= 0) return ""
    val moment = Instant.ofEpochMilli(value).atZone(zone)
    val today = Instant.ofEpochMilli(now).atZone(zone).toLocalDate()
    val day = moment.toLocalDate()
    val time = DateTimeFormatter.ofPattern("HH:mm").format(moment)
    val label =
        when {
            day == today -> "Aujourd’hui"
            day == today.plusDays(1) -> "Demain"
            day.year == today.year ->
                DateTimeFormatter.ofPattern("EEE d MMM", Locale.FRENCH).format(moment)
            else -> DateTimeFormatter.ofPattern("EEE d MMM yyyy", Locale.FRENCH).format(moment)
        }
    return "$label · $time"
}

// ---------- Navigation ----------

internal data class DockItem(val route: String, val label: String, val icon: ImageVector)

internal val DockItems =
    listOf(
        DockItem("fil", "Fil", CairnIcons.Home),
        DockItem("missions", "Missions", CairnIcons.Orbit),
        DockItem("atelier", "Atelier", CairnIcons.Grid),
    )

/** Floating ink dock with the three destinations and the always-available "new" action. */
@Composable
internal fun CairnDock(
    selected: String,
    navigate: (String) -> Unit,
    create: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Row(
        modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(
            Modifier.weight(1f)
                .height(60.dp)
                .clip(CircleShape)
                .background(signal.dock)
                .padding(6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            DockItems.forEach { item ->
                val on = item.route == selected
                Row(
                    Modifier.weight(if (on) 1.35f else 1f)
                        .fillMaxHeight()
                        .clip(CircleShape)
                        .background(
                            if (on) MaterialTheme.colorScheme.primary else Color.Transparent
                        )
                        .selectable(on, role = Role.Tab) { navigate(item.route) },
                    horizontalArrangement = Arrangement.Center,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    // Label once: the icon names the tab until the selected tab shows its text.
                    Icon(
                        item.icon,
                        if (on) null else item.label,
                        Modifier.size(22.dp),
                        tint = if (on) MaterialTheme.colorScheme.onPrimary else signal.onDock,
                    )
                    if (on) {
                        Spacer(Modifier.width(8.dp))
                        Text(
                            item.label,
                            style = MaterialTheme.typography.labelLarge,
                            fontWeight = FontWeight.Bold,
                            color = MaterialTheme.colorScheme.onPrimary,
                            maxLines = 1,
                        )
                    }
                }
            }
        }
        RoundAction(
            "Nouvelle conversation",
            CairnIcons.Plus,
            container = MaterialTheme.colorScheme.primary,
            content = MaterialTheme.colorScheme.onPrimary,
            outlined = false,
            size = 60.dp,
            onClick = create,
        )
    }
}

// ---------- Extra icons, same stroke family as CairnIcons ----------

internal object SignalIcons {
    fun line(name: String, width: Float = 1.8f, draw: PathBuilder.() -> Unit) =
        ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
            .apply {
                path(
                    fill = null,
                    stroke = SolidColor(Color.Black),
                    strokeLineWidth = width,
                    strokeLineCap = StrokeCap.Round,
                    strokeLineJoin = StrokeJoin.Round,
                    pathBuilder = draw,
                )
            }
            .build()

    fun solid(name: String, draw: PathBuilder.() -> Unit) =
        ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
            .apply { path(fill = SolidColor(Color.Black), pathBuilder = draw) }
            .build()
}
