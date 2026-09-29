@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package dev.leo.manager.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.PathBuilder
import androidx.compose.ui.graphics.vector.path
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

// Small shared outline family, with native 48 dp targets and accessible labels.
internal object LeoIcons {
    private fun line(name: String, draw: PathBuilder.() -> Unit) =
        ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
            .apply {
                path(
                    fill = null,
                    stroke = SolidColor(Color.Black),
                    strokeLineWidth = 1.8f,
                    strokeLineCap = StrokeCap.Round,
                    strokeLineJoin = StrokeJoin.Round,
                    pathBuilder = draw,
                )
            }
            .build()

    val Chat =
        line("Chat") {
            moveTo(5f, 4f)
            lineTo(19f, 4f)
            quadTo(21f, 4f, 21f, 6f)
            lineTo(21f, 16f)
            quadTo(21f, 18f, 19f, 18f)
            lineTo(8f, 18f)
            lineTo(3f, 21f)
            lineTo(3f, 6f)
            quadTo(3f, 4f, 5f, 4f)
        }
    val Book =
        line("Book") {
            moveTo(12f, 7f)
            quadTo(12f, 3f, 8f, 3f)
            lineTo(3f, 3f)
            lineTo(3f, 18f)
            lineTo(9f, 18f)
            quadTo(12f, 18f, 12f, 21f)
            quadTo(12f, 18f, 15f, 18f)
            lineTo(21f, 18f)
            lineTo(21f, 3f)
            lineTo(16f, 3f)
            quadTo(12f, 3f, 12f, 7f)
            lineTo(12f, 21f)
        }
    val Tasks =
        line("Tasks") {
            moveTo(9f, 5f)
            lineTo(21f, 5f)
            moveTo(9f, 12f)
            lineTo(21f, 12f)
            moveTo(9f, 19f)
            lineTo(21f, 19f)
            moveTo(2f, 5f)
            lineTo(4f, 7f)
            lineTo(7f, 3f)
            moveTo(2f, 12f)
            lineTo(4f, 14f)
            lineTo(7f, 10f)
            moveTo(3f, 19f)
            lineTo(5f, 19f)
        }
    val File =
        line("File") {
            moveTo(14f, 3f)
            lineTo(5f, 3f)
            lineTo(5f, 21f)
            lineTo(19f, 21f)
            lineTo(19f, 8f)
            lineTo(14f, 3f)
            lineTo(14f, 8f)
            lineTo(19f, 8f)
            moveTo(9f, 13f)
            lineTo(15f, 13f)
            moveTo(9f, 17f)
            lineTo(13f, 17f)
        }
    val Layers =
        line("Layers") {
            moveTo(3f, 7f)
            lineTo(12f, 3f)
            lineTo(21f, 7f)
            lineTo(12f, 11f)
            close()
            moveTo(3f, 12f)
            lineTo(12f, 16f)
            lineTo(21f, 12f)
            moveTo(3f, 17f)
            lineTo(12f, 21f)
            lineTo(21f, 17f)
        }
    val Down =
        line("Down") {
            moveTo(6f, 9f)
            lineTo(12f, 15f)
            lineTo(18f, 9f)
        }
    val Right =
        line("Right") {
            moveTo(9f, 6f)
            lineTo(15f, 12f)
            lineTo(9f, 18f)
        }
    val Up =
        line("Send") {
            moveTo(12f, 19f)
            lineTo(12f, 5f)
            moveTo(6f, 11f)
            lineTo(12f, 5f)
            lineTo(18f, 11f)
        }
    val Bottom =
        line("Latest") {
            moveTo(12f, 5f)
            lineTo(12f, 19f)
            moveTo(6f, 13f)
            lineTo(12f, 19f)
            lineTo(18f, 13f)
        }
    val Pause =
        line("Pause") {
            moveTo(8f, 5f)
            lineTo(8f, 19f)
            moveTo(16f, 5f)
            lineTo(16f, 19f)
        }
    val Steer =
        line("Steer") {
            moveTo(13f, 2f)
            lineTo(4f, 14f)
            lineTo(11f, 14f)
            lineTo(10f, 22f)
            lineTo(20f, 10f)
            lineTo(13f, 10f)
            close()
        }
    val Stop =
        line("Stop") {
            moveTo(6f, 6f)
            lineTo(18f, 6f)
            lineTo(18f, 18f)
            lineTo(6f, 18f)
            close()
        }
    val Tune =
        line("Tune") {
            moveTo(4f, 7f)
            lineTo(8f, 7f)
            moveTo(12f, 7f)
            lineTo(20f, 7f)
            moveTo(10f, 4f)
            lineTo(10f, 10f)
            moveTo(4f, 17f)
            lineTo(12f, 17f)
            moveTo(16f, 17f)
            lineTo(20f, 17f)
            moveTo(14f, 14f)
            lineTo(14f, 20f)
        }
    val Terminal =
        line("Terminal") {
            moveTo(4f, 6f)
            lineTo(10f, 12f)
            lineTo(4f, 18f)
            moveTo(13f, 18f)
            lineTo(20f, 18f)
        }
    val Download =
        line("Download") {
            moveTo(12f, 3f)
            lineTo(12f, 15f)
            moveTo(7f, 10f)
            lineTo(12f, 15f)
            lineTo(17f, 10f)
            moveTo(4f, 16f)
            lineTo(4f, 21f)
            lineTo(20f, 21f)
            lineTo(20f, 16f)
        }
    val External =
        line("External") {
            moveTo(14f, 3f)
            lineTo(21f, 3f)
            lineTo(21f, 10f)
            moveTo(21f, 3f)
            lineTo(10f, 14f)
            moveTo(10f, 5f)
            lineTo(4f, 5f)
            lineTo(4f, 21f)
            lineTo(19f, 21f)
            lineTo(19f, 15f)
        }
    // "Signal" navigation and action icons.
    val Home =
        line("Fil") {
            moveTo(4f, 11f)
            lineTo(12f, 4f)
            lineTo(20f, 11f)
            lineTo(20f, 20f)
            lineTo(14.5f, 20f)
            lineTo(14.5f, 14f)
            lineTo(9.5f, 14f)
            lineTo(9.5f, 20f)
            lineTo(4f, 20f)
            close()
        }
    val Orbit =
        line("Missions") {
            moveTo(12f, 3f)
            arcTo(9f, 9f, 0f, true, true, 3f, 12f)
            moveTo(12f, 8f)
            arcTo(4f, 4f, 0f, true, true, 8f, 12f)
            moveTo(12f, 12f)
            lineTo(19f, 5f)
            moveTo(15.5f, 4.5f)
            lineTo(19.5f, 4.5f)
            lineTo(19.5f, 8.5f)
        }
    val Grid =
        line("Atelier") {
            moveTo(4f, 4f)
            lineTo(10f, 4f)
            lineTo(10f, 10f)
            lineTo(4f, 10f)
            close()
            moveTo(14f, 4f)
            lineTo(20f, 4f)
            lineTo(20f, 10f)
            lineTo(14f, 10f)
            close()
            moveTo(4f, 14f)
            lineTo(10f, 14f)
            lineTo(10f, 20f)
            lineTo(4f, 20f)
            close()
            moveTo(17f, 14f)
            lineTo(17f, 20f)
            moveTo(14f, 17f)
            lineTo(20f, 17f)
        }
    val Plus =
        SignalIcons.line("Plus", 2.2f) {
            moveTo(12f, 5f)
            lineTo(12f, 19f)
            moveTo(5f, 12f)
            lineTo(19f, 12f)
        }
    val Search =
        line("Search") {
            moveTo(11f, 4f)
            arcTo(7f, 7f, 0f, true, true, 10.99f, 4f)
            moveTo(16f, 16f)
            lineTo(20.5f, 20.5f)
        }
    val Retry =
        line("Retry") {
            moveTo(19.5f, 12f)
            arcTo(7.5f, 7.5f, 0f, true, true, 17f, 6.4f)
            moveTo(17.5f, 3f)
            lineTo(17.5f, 7f)
            lineTo(13.5f, 7f)
        }
    val Clock =
        line("Clock") {
            moveTo(12f, 3.5f)
            arcTo(8.5f, 8.5f, 0f, true, true, 11.99f, 3.5f)
            moveTo(12f, 7.5f)
            lineTo(12f, 12f)
            lineTo(15f, 14f)
        }
    val Play =
        SignalIcons.solid("Play") {
            moveTo(8f, 5.5f)
            lineTo(18.5f, 12f)
            lineTo(8f, 18.5f)
            close()
        }
    val StopSolid =
        SignalIcons.solid("Stop") {
            moveTo(7f, 7f)
            lineTo(17f, 7f)
            lineTo(17f, 17f)
            lineTo(7f, 17f)
            close()
        }
    val Pencil =
        line("Edit") {
            moveTo(4f, 20f)
            lineTo(8f, 19f)
            lineTo(19f, 8f)
            lineTo(16f, 5f)
            lineTo(5f, 16f)
            close()
        }
    val Gear =
        line("Settings") {
            moveTo(12f, 9f)
            arcTo(3f, 3f, 0f, true, true, 11.99f, 9f)
            moveTo(12f, 2.8f)
            lineTo(12f, 5f)
            moveTo(12f, 19f)
            lineTo(12f, 21.2f)
            moveTo(2.8f, 12f)
            lineTo(5f, 12f)
            moveTo(19f, 12f)
            lineTo(21.2f, 12f)
            moveTo(5.5f, 5.5f)
            lineTo(7f, 7f)
            moveTo(17f, 17f)
            lineTo(18.5f, 18.5f)
            moveTo(5.5f, 18.5f)
            lineTo(7f, 17f)
            moveTo(17f, 7f)
            lineTo(18.5f, 5.5f)
        }
    val Key =
        line("Key") {
            moveTo(8f, 11f)
            arcTo(4f, 4f, 0f, true, true, 7.99f, 11f)
            moveTo(11.5f, 13f)
            lineTo(20f, 13f)
            lineTo(20f, 16f)
            moveTo(16.5f, 13f)
            lineTo(16.5f, 15.5f)
        }
    val Shield =
        line("Authorize") {
            moveTo(12f, 3f)
            lineTo(19.5f, 6f)
            lineTo(19.5f, 12f)
            quadTo(19.5f, 18f, 12f, 21f)
            quadTo(4.5f, 18f, 4.5f, 12f)
            lineTo(4.5f, 6f)
            close()
            moveTo(9f, 12f)
            lineTo(11f, 14f)
            lineTo(15f, 10f)
        }
    val Log =
        line("Journal") {
            moveTo(8f, 6f)
            lineTo(20f, 6f)
            moveTo(8f, 12f)
            lineTo(20f, 12f)
            moveTo(8f, 18f)
            lineTo(20f, 18f)
            moveTo(4f, 6f)
            lineTo(4.2f, 6f)
            moveTo(4f, 12f)
            lineTo(4.2f, 12f)
            moveTo(4f, 18f)
            lineTo(4.2f, 18f)
        }
    val Spark =
        line("Skill") {
            moveTo(12f, 3f)
            quadTo(13f, 11f, 21f, 12f)
            quadTo(13f, 13f, 12f, 21f)
            quadTo(11f, 13f, 3f, 12f)
            quadTo(11f, 11f, 12f, 3f)
        }
    val Folder =
        line("Project") {
            moveTo(3f, 6f)
            lineTo(10f, 6f)
            lineTo(12f, 8.5f)
            lineTo(21f, 8.5f)
            lineTo(21f, 19f)
            lineTo(3f, 19f)
            close()
        }
    val Plug =
        line("Plug") {
            moveTo(9f, 3f)
            lineTo(9f, 7f)
            moveTo(15f, 3f)
            lineTo(15f, 7f)
            moveTo(6f, 7f)
            lineTo(18f, 7f)
            lineTo(18f, 11f)
            arcTo(6f, 6f, 0f, false, true, 6f, 11f)
            close()
            moveTo(12f, 17f)
            lineTo(12f, 21f)
        }
    val Check =
        SignalIcons.line("Check", 2.2f) {
            moveTo(5f, 12.5f)
            lineTo(10f, 17.5f)
            lineTo(19f, 7f)
        }
    val Close =
        line("Close") {
            moveTo(6f, 6f)
            lineTo(18f, 18f)
            moveTo(18f, 6f)
            lineTo(6f, 18f)
        }
    val Back =
        line("Back") {
            moveTo(15f, 5f)
            lineTo(8f, 12f)
            lineTo(15f, 19f)
        }
    val Alert =
        line("Alert") {
            moveTo(12f, 3.5f)
            lineTo(21.5f, 20f)
            lineTo(2.5f, 20f)
            close()
            moveTo(12f, 10f)
            lineTo(12f, 14f)
            moveTo(12f, 17f)
            lineTo(12f, 17.2f)
        }
    val Attach =
        line("Attach") {
            moveTo(20f, 11.5f)
            lineTo(12f, 19.5f)
            arcTo(4.6f, 4.6f, 0f, false, true, 5.5f, 13f)
            lineTo(13.5f, 5f)
            arcTo(3f, 3f, 0f, false, true, 17.8f, 9.2f)
            lineTo(10f, 17f)
            arcTo(1.5f, 1.5f, 0f, false, true, 7.9f, 14.9f)
            lineTo(15f, 7.8f)
        }
    val More =
        SignalIcons.solid("More") {
            for (x in listOf(5f, 12f, 19f)) {
                moveTo(x + 1.7f, 12f)
                arcTo(1.7f, 1.7f, 0f, true, true, x - 1.7f, 12f)
                arcTo(1.7f, 1.7f, 0f, true, true, x + 1.7f, 12f)
            }
        }
}

@Composable
internal fun ActionIcon(
    label: String,
    icon: ImageVector,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    TooltipBox(
        positionProvider = TooltipDefaults.rememberPlainTooltipPositionProvider(),
        tooltip = { PlainTooltip { Text(label) } },
        state = rememberTooltipState(),
    ) {
        IconButton(onClick = onClick, enabled = enabled) { Icon(icon, label, Modifier.size(22.dp)) }
    }
}

@Composable
internal fun SearchField(label: String, value: String, change: (String) -> Unit) {
    TextField(
        value,
        change,
        Modifier.fillMaxWidth(),
        placeholder = { Text(label, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        keyboardOptions = InputKeyboards.Search,
        singleLine = true,
        shape = RoundedCornerShape(12.dp),
        textStyle = MaterialTheme.typography.bodyMedium,
        colors =
            TextFieldDefaults.colors(
                focusedIndicatorColor = Color.Transparent,
                unfocusedIndicatorColor = Color.Transparent,
                focusedContainerColor = MaterialTheme.colorScheme.surfaceVariant,
                unfocusedContainerColor = MaterialTheme.colorScheme.surfaceVariant,
            ),
    )
}

@Composable
internal fun DetailHeader(
    title: String,
    subtitle: String,
    back: () -> Unit,
    actions: @Composable RowScope.() -> Unit,
) {
    androidx.compose.material3.TopAppBar(
        title = {
            Column {
                Text(
                    title,
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (subtitle.isNotBlank())
                    Text(
                        subtitle,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
            }
        },
        navigationIcon = {
            ActionIcon("Retour", LeoIcons.Back, onClick = back)
        },
        actions = actions,
        windowInsets = WindowInsets(0, 0, 0, 0),
        colors =
            TopAppBarDefaults.topAppBarColors(
                containerColor = MaterialTheme.colorScheme.background
            ),
    )
}
