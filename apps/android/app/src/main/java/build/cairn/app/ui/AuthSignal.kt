package build.cairn.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.rotate
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.PathBuilder
import androidx.compose.ui.graphics.vector.path
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import build.cairn.app.data.AccountProvider

internal const val EMAIL_CODE_LENGTH = 8

/** « Signal Pur » backdrop: warm paper or obsidian canvas with a subtle blue/amber beacon glow. */
@Composable
internal fun AuthBackdrop(modifier: Modifier = Modifier, content: @Composable BoxScope.() -> Unit) {
    val background = MaterialTheme.colorScheme.background
    val dark = background.luminance() < 0.25f
    val blueGlow =
        if (dark) Color(0xFF2F6EC7).copy(alpha = 0.16f) else Color(0xFF14365A).copy(alpha = 0.08f)
    val amberGlow =
        if (dark) Color(0xFFF2A93B).copy(alpha = 0.10f) else Color(0xFFB8650A).copy(alpha = 0.09f)
    val contourColor =
        MaterialTheme.colorScheme.onBackground.copy(alpha = if (dark) 0.055f else 0.06f)

    Box(modifier.fillMaxSize().background(background)) {
        Canvas(Modifier.fillMaxSize()) {
            val center = Offset(size.width * 0.5f, size.height * 0.16f)
            drawRect(
                Brush.radialGradient(
                    colors = listOf(blueGlow, Color.Transparent),
                    center = center,
                    radius = size.maxDimension * 0.55f,
                )
            )
            drawRect(
                Brush.radialGradient(
                    colors = listOf(amberGlow, Color.Transparent),
                    center = Offset(size.width * 0.58f, size.height * 0.10f),
                    radius = size.maxDimension * 0.36f,
                )
            )
            val contourCenter = Offset(size.width * 0.5f, size.height * 0.24f)
            rotate(degrees = -14f, pivot = contourCenter) {
                listOf(140f, 220f, 310f, 410f, 520f).forEach { rx ->
                    val rxPx = rx.dp.toPx()
                    val ryPx = (rx * 0.65f).dp.toPx()
                    drawOval(
                        color = contourColor,
                        topLeft = Offset(contourCenter.x - rxPx, contourCenter.y - ryPx),
                        size = Size(rxPx * 2f, ryPx * 2f),
                        style = Stroke(width = 1.dp.toPx()),
                    )
                }
            }
        }
        content()
    }
}

@Composable
internal fun AuthCard(
    modifier: Modifier = Modifier,
    padding: PaddingValues = PaddingValues(20.dp),
    content: @Composable ColumnScope.() -> Unit,
) {
    Surface(
        modifier = modifier.fillMaxWidth(),
        shape = RoundedCornerShape(20.dp),
        color = MaterialTheme.colorScheme.surface,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column(
            Modifier.padding(padding),
            verticalArrangement = Arrangement.spacedBy(14.dp),
            content = content,
        )
    }
}

@Composable
internal fun AuthDivider(label: String, modifier: Modifier = Modifier) {
    Row(
        modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        HorizontalDivider(Modifier.weight(1f), color = MaterialTheme.colorScheme.outlineVariant)
        Text(
            label,
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        HorizontalDivider(Modifier.weight(1f), color = MaterialTheme.colorScheme.outlineVariant)
    }
}

/**
 * Single real text input backing 8 visual OTP boxes so autofill, paste, keyboard navigation and
 * accessibility/UI tests targeting [label] work natively.
 */
@Composable
internal fun OtpCodeField(
    label: String,
    value: String,
    onValueChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
) {
    var focused by remember { mutableStateOf(false) }
    var field by remember { mutableStateOf(TextFieldValue()) }
    val input = if (field.text == value) field else TextFieldValue(value, TextRange(value.length))
    val selection = input.selection
    val activeIndex = selection.start.coerceAtMost(EMAIL_CODE_LENGTH - 1)

    BasicTextField(
        value = input,
        onValueChange = { change ->
            val digits = change.text.filter { it in '0'..'9' }.take(EMAIL_CODE_LENGTH)
            if (digits == change.text) {
                field = change
            } else {
                fun digitOffset(index: Int) =
                    change.text.take(index).count { it in '0'..'9' }.coerceAtMost(digits.length)

                field =
                    TextFieldValue(
                        digits,
                        TextRange(
                            digitOffset(change.selection.start),
                            digitOffset(change.selection.end),
                        ),
                    )
            }
            onValueChange(digits)
        },
        enabled = enabled,
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        textStyle = TextStyle(color = Color.Transparent, fontSize = 1.sp),
        cursorBrush = SolidColor(Color.Transparent),
        modifier = modifier.fillMaxWidth().onFocusChanged { focused = it.isFocused },
        decorationBox = { innerTextField ->
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    label,
                    style = MaterialTheme.typography.labelLarge,
                    fontWeight = FontWeight.SemiBold,
                    color = MaterialTheme.colorScheme.onSurface,
                )
                Box(Modifier.fillMaxWidth()) {
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        repeat(EMAIL_CODE_LENGTH) { index ->
                            val char = value.getOrNull(index)?.toString().orEmpty()
                            val isCurrent = focused && selection.collapsed && index == activeIndex
                            val isSelected = focused && index in selection.min until selection.max
                            val borderColor =
                                when {
                                    isCurrent || isSelected -> MaterialTheme.colorScheme.primary
                                    char.isNotEmpty() -> MaterialTheme.colorScheme.outlineVariant
                                    else -> MaterialTheme.colorScheme.outline.copy(alpha = 0.55f)
                                }
                            val fillColor =
                                when {
                                    isSelected -> MaterialTheme.colorScheme.primaryContainer
                                    isCurrent || char.isNotEmpty() ->
                                        MaterialTheme.colorScheme.surface
                                    else -> MaterialTheme.colorScheme.surfaceContainer
                                }

                            Box(
                                Modifier.weight(1f)
                                    .height(48.dp)
                                    .clip(RoundedCornerShape(11.dp))
                                    .background(fillColor)
                                    .border(
                                        width = if (isCurrent || isSelected) 2.dp else 1.dp,
                                        color = borderColor,
                                        shape = RoundedCornerShape(11.dp),
                                    ),
                                contentAlignment = Alignment.Center,
                            ) {
                                if (char.isNotEmpty()) {
                                    Text(
                                        char,
                                        style = MaterialTheme.typography.titleLarge,
                                        fontWeight = FontWeight.Bold,
                                        color = MaterialTheme.colorScheme.onSurface,
                                    )
                                }
                                if (isCurrent) {
                                    val caretAlignment =
                                        when {
                                            selection.start == EMAIL_CODE_LENGTH ->
                                                Alignment.CenterEnd
                                            char.isNotEmpty() -> Alignment.CenterStart
                                            else -> Alignment.Center
                                        }
                                    Box(
                                        Modifier.align(caretAlignment)
                                            .padding(horizontal = 4.dp)
                                            .width(2.dp)
                                            .height(20.dp)
                                            .clip(RoundedCornerShape(2.dp))
                                            .background(MaterialTheme.colorScheme.primary)
                                    )
                                }
                            }
                        }
                    }
                    Box(Modifier.matchParentSize().alpha(0f)) { innerTextField() }
                }
            }
        },
    )
}

@Composable
internal fun SocialProviderIcon(provider: AccountProvider, modifier: Modifier = Modifier) {
    when (provider) {
        AccountProvider.Google -> GoogleMark(modifier.size(18.dp))
        AccountProvider.GitHub ->
            Icon(AuthIcons.GitHub, null, modifier.size(18.dp), tint = LocalContentColor.current)
        AccountProvider.Passkey ->
            Icon(AuthIcons.Passkey, null, modifier.size(18.dp), tint = LocalContentColor.current)
    }
}

@Composable
private fun GoogleMark(modifier: Modifier = Modifier) {
    Canvas(modifier) {
        val stroke = size.minDimension * 0.19f
        val inset = stroke / 2f
        val arcSize = Size(size.width - stroke, size.height - stroke)
        val topLeft = Offset(inset, inset)
        drawArc(
            color = Color(0xFFEA4335),
            startAngle = 215f,
            sweepAngle = 100f,
            useCenter = false,
            topLeft = topLeft,
            size = arcSize,
            style = Stroke(stroke, cap = StrokeCap.Butt),
        )
        drawArc(
            color = Color(0xFFFBBC05),
            startAngle = 140f,
            sweepAngle = 78f,
            useCenter = false,
            topLeft = topLeft,
            size = arcSize,
            style = Stroke(stroke, cap = StrokeCap.Butt),
        )
        drawArc(
            color = Color(0xFF34A853),
            startAngle = 42f,
            sweepAngle = 100f,
            useCenter = false,
            topLeft = topLeft,
            size = arcSize,
            style = Stroke(stroke, cap = StrokeCap.Butt),
        )
        drawArc(
            color = Color(0xFF4285F4),
            startAngle = -8f,
            sweepAngle = 54f,
            useCenter = false,
            topLeft = topLeft,
            size = arcSize,
            style = Stroke(stroke, cap = StrokeCap.Butt),
        )
        val midY = size.height * 0.51f
        drawPath(
            path =
                Path().apply {
                    moveTo(size.width * 0.52f, midY)
                    lineTo(size.width * 0.94f, midY)
                },
            color = Color(0xFF4285F4),
            style = Stroke(stroke, cap = StrokeCap.Butt),
        )
    }
}

internal object AuthIcons {
    private fun stroke(name: String, draw: PathBuilder.() -> Unit) =
        ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
            .apply {
                path(
                    fill = null,
                    stroke = SolidColor(Color.Black),
                    strokeLineWidth = 1.9f,
                    strokeLineCap = StrokeCap.Round,
                    strokeLineJoin = StrokeJoin.Round,
                    pathBuilder = draw,
                )
            }
            .build()

    val Passkey =
        stroke("Passkey") {
            moveTo(13f, 7f)
            arcTo(4f, 4f, 0f, true, true, 5f, 7f)
            arcTo(4f, 4f, 0f, true, true, 13f, 7f)
            moveTo(2f, 21f)
            verticalLineTo(19f)
            quadTo(2f, 15f, 6f, 15f)
            horizontalLineTo(10f)
            moveTo(19.5f, 14f)
            arcTo(2.5f, 2.5f, 0f, true, true, 14.5f, 14f)
            arcTo(2.5f, 2.5f, 0f, true, true, 19.5f, 14f)
            moveTo(17f, 16.5f)
            verticalLineTo(22f)
            moveTo(17f, 20f)
            horizontalLineTo(19.5f)
        }

    val GitHub =
        ImageVector.Builder("GitHub", 24.dp, 24.dp, 24f, 24f)
            .apply {
                path(fill = SolidColor(Color.Black)) {
                    moveTo(12f, 0.5f)
                    curveTo(5.65f, 0.5f, 0.5f, 5.65f, 0.5f, 12f)
                    curveTo(0.5f, 17.08f, 3.79f, 21.39f, 8.36f, 22.91f)
                    curveTo(8.94f, 23.02f, 9.15f, 22.66f, 9.15f, 22.35f)
                    curveTo(9.15f, 22.08f, 9.14f, 21.18f, 9.13f, 20.23f)
                    curveTo(5.93f, 20.93f, 5.25f, 18.87f, 5.25f, 18.87f)
                    curveTo(4.73f, 17.54f, 3.97f, 17.18f, 3.97f, 17.18f)
                    curveTo(2.92f, 16.46f, 4.05f, 16.48f, 4.05f, 16.48f)
                    curveTo(5.21f, 16.56f, 5.82f, 17.67f, 5.82f, 17.67f)
                    curveTo(6.85f, 19.43f, 8.52f, 18.92f, 9.18f, 18.63f)
                    curveTo(9.28f, 17.88f, 9.58f, 17.38f, 9.91f, 17.09f)
                    curveTo(7.36f, 16.8f, 4.67f, 15.81f, 4.67f, 11.41f)
                    curveTo(4.67f, 10.16f, 5.12f, 9.13f, 5.86f, 8.33f)
                    curveTo(5.74f, 8.04f, 5.34f, 6.87f, 5.97f, 5.29f)
                    curveTo(5.97f, 5.29f, 6.94f, 4.98f, 9.14f, 6.47f)
                    curveTo(10.06f, 6.21f, 11.04f, 6.09f, 12.02f, 6.09f)
                    curveTo(13f, 6.09f, 13.98f, 6.21f, 14.9f, 6.47f)
                    curveTo(17.1f, 4.98f, 18.07f, 5.29f, 18.07f, 5.29f)
                    curveTo(18.7f, 6.87f, 18.3f, 8.04f, 18.18f, 8.33f)
                    curveTo(18.92f, 9.13f, 19.37f, 10.16f, 19.37f, 11.41f)
                    curveTo(19.37f, 15.82f, 16.68f, 16.8f, 14.12f, 17.08f)
                    curveTo(14.53f, 17.44f, 14.9f, 18.14f, 14.9f, 19.22f)
                    curveTo(14.9f, 20.77f, 14.89f, 22.02f, 14.89f, 22.4f)
                    curveTo(14.89f, 22.71f, 15.1f, 23.08f, 15.69f, 22.96f)
                    curveTo(20.23f, 21.44f, 23.5f, 17.13f, 23.5f, 12f)
                    curveTo(23.5f, 5.65f, 18.35f, 0.5f, 12f, 0.5f)
                    close()
                }
            }
            .build()

    val Eye =
        stroke("Eye") {
            moveTo(2f, 12f)
            quadTo(6f, 5f, 12f, 5f)
            quadTo(18f, 5f, 22f, 12f)
            quadTo(18f, 19f, 12f, 19f)
            quadTo(6f, 19f, 2f, 12f)
            moveTo(15f, 12f)
            arcTo(3f, 3f, 0f, true, true, 9f, 12f)
            arcTo(3f, 3f, 0f, true, true, 15f, 12f)
        }

    val Terminal =
        stroke("Terminal") {
            moveTo(4f, 17f)
            lineTo(10f, 11f)
            lineTo(4f, 5f)
            moveTo(12f, 19f)
            horizontalLineTo(20f)
        }

    val Pencil =
        stroke("Pencil") {
            moveTo(12f, 20f)
            horizontalLineTo(21f)
            moveTo(16.5f, 3.5f)
            lineTo(20.5f, 7.5f)
            lineTo(7f, 21f)
            lineTo(3f, 21f)
            lineTo(3f, 17f)
            close()
        }

    val Link =
        stroke("Link") {
            moveTo(10f, 13f)
            quadTo(13f, 16f, 16f, 13f)
            lineTo(19f, 10f)
            quadTo(22f, 7f, 19f, 4f)
            quadTo(16f, 1f, 13f, 4f)
            lineTo(11.5f, 5.5f)
            moveTo(14f, 11f)
            quadTo(11f, 8f, 8f, 11f)
            lineTo(5f, 14f)
            quadTo(2f, 17f, 5f, 20f)
            quadTo(8f, 23f, 11f, 20f)
            lineTo(12.5f, 18.5f)
        }

    val Lock =
        stroke("Lock") {
            moveTo(5f, 11f)
            horizontalLineTo(19f)
            quadTo(21f, 11f, 21f, 13f)
            verticalLineTo(19f)
            quadTo(21f, 21f, 19f, 21f)
            horizontalLineTo(5f)
            quadTo(3f, 21f, 3f, 19f)
            verticalLineTo(13f)
            quadTo(3f, 11f, 5f, 11f)
            moveTo(7f, 11f)
            verticalLineTo(7f)
            arcTo(5f, 5f, 0f, false, true, 17f, 7f)
            verticalLineTo(11f)
        }

    val Laptop =
        stroke("Laptop") {
            moveTo(4f, 5f)
            horizontalLineTo(20f)
            verticalLineTo(16f)
            horizontalLineTo(4f)
            close()
            moveTo(2f, 20f)
            horizontalLineTo(22f)
        }

    val Warning =
        stroke("Warning") {
            moveTo(12f, 3f)
            lineTo(22f, 20f)
            horizontalLineTo(2f)
            close()
            moveTo(12f, 9f)
            verticalLineTo(13f)
            moveTo(12f, 17f)
            horizontalLineTo(12.01f)
        }

    val ShieldCheck =
        stroke("ShieldCheck") {
            moveTo(12f, 2f)
            lineTo(20f, 5f)
            verticalLineTo(12f)
            quadTo(20f, 18f, 12f, 22f)
            quadTo(4f, 18f, 4f, 12f)
            verticalLineTo(5f)
            close()
            moveTo(9f, 12f)
            lineTo(11f, 14f)
            lineTo(15f, 10f)
        }
}
