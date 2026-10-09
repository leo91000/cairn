package build.cairn.app.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import build.cairn.app.R

private val CairnBody =
    FontFamily(
        Font(R.font.cairn_body_400, FontWeight.Normal),
        Font(R.font.cairn_body_500, FontWeight.Medium),
        Font(R.font.cairn_body_600, FontWeight.SemiBold),
        Font(R.font.cairn_body_700, FontWeight.Bold),
    )
private val CairnHeading =
    FontFamily(
        Font(R.font.cairn_heading_600, FontWeight.SemiBold),
        Font(R.font.cairn_heading_700, FontWeight.Bold),
    )

// The web's DM Sans / Manrope identity, with Android font scaling intact.
private val CairnTypography =
    Typography().let { base ->
        base.copy(
            displayLarge = base.displayLarge.copy(fontFamily = CairnHeading),
            displayMedium = base.displayMedium.copy(fontFamily = CairnHeading),
            displaySmall = base.displaySmall.copy(fontFamily = CairnHeading),
            // Screen titles: large, bold and slightly tightened for character.
            headlineLarge =
                base.headlineLarge.copy(
                    fontFamily = CairnHeading,
                    fontSize = 32.sp,
                    lineHeight = 36.sp,
                    fontWeight = FontWeight.Bold,
                    letterSpacing = (-0.8).sp,
                ),
            headlineMedium =
                base.headlineMedium.copy(
                    fontFamily = CairnHeading,
                    fontSize = 26.sp,
                    lineHeight = 31.sp,
                    fontWeight = FontWeight.Bold,
                    letterSpacing = (-0.6).sp,
                ),
            headlineSmall =
                base.headlineSmall.copy(
                    fontFamily = CairnHeading,
                    fontSize = 24.sp,
                    lineHeight = 29.sp,
                    fontWeight = FontWeight.Bold,
                    letterSpacing = (-0.4).sp,
                ),
            titleLarge =
                base.titleLarge.copy(
                    fontFamily = CairnHeading,
                    fontSize = 20.sp,
                    lineHeight = 28.sp,
                    fontWeight = FontWeight.SemiBold,
                ),
            titleMedium =
                base.titleMedium.copy(
                    fontFamily = CairnHeading,
                    fontSize = 16.sp,
                    lineHeight = 22.sp,
                    fontWeight = FontWeight.SemiBold,
                ),
            titleSmall =
                base.titleSmall.copy(
                    fontFamily = CairnBody,
                    fontSize = 14.sp,
                    lineHeight = 20.sp,
                    fontWeight = FontWeight.SemiBold,
                ),
            bodyLarge =
                base.bodyLarge.copy(
                    fontFamily = CairnBody,
                    fontSize = 16.sp,
                    lineHeight = 24.sp,
                    letterSpacing = 0.sp,
                ),
            bodyMedium =
                base.bodyMedium.copy(
                    fontFamily = CairnBody,
                    fontSize = 14.sp,
                    lineHeight = 20.sp,
                    letterSpacing = 0.sp,
                ),
            bodySmall =
                base.bodySmall.copy(fontFamily = CairnBody, fontSize = 12.sp, lineHeight = 16.sp),
            labelLarge =
                base.labelLarge.copy(fontFamily = CairnBody, fontSize = 13.sp, lineHeight = 18.sp),
            labelMedium =
                base.labelMedium.copy(fontFamily = CairnBody, fontSize = 12.sp, lineHeight = 16.sp),
            labelSmall =
                base.labelSmall.copy(fontFamily = CairnBody, fontSize = 11.sp, lineHeight = 14.sp),
        )
    }

// "Signal" palette: warm paper and ink, with the Cairn blue as the single accent colour for
// actions, selection and live work. Coral is reserved for things that need the user.
internal val CairnLightColors =
    lightColorScheme(
        primary = Color(0xFF14365A),
        onPrimary = Color.White,
        primaryContainer = Color(0xFFE4EDF6),
        onPrimaryContainer = Color(0xFF14365A),
        inversePrimary = Color(0xFF8DB8E8),
        secondary = Color(0xFF6D6A78),
        onSecondary = Color.White,
        secondaryContainer = Color(0xFFE4EDF6),
        onSecondaryContainer = Color(0xFF16151D),
        tertiary = Color(0xFF8986A4),
        onTertiary = Color.White,
        tertiaryContainer = Color(0xFFECE9E1),
        onTertiaryContainer = Color(0xFF16151D),
        background = Color(0xFFF4F2EC),
        onBackground = Color(0xFF16151D),
        surface = Color(0xFFFFFFFF),
        onSurface = Color(0xFF16151D),
        surfaceVariant = Color(0xFFECE9E1),
        onSurfaceVariant = Color(0xFF6D6A78),
        surfaceTint = Color(0xFF14365A),
        inverseSurface = Color(0xFF16151D),
        inverseOnSurface = Color(0xFFF4F2EC),
        surfaceBright = Color.White,
        surfaceDim = Color(0xFFE3DFD5),
        surfaceContainerLowest = Color.White,
        surfaceContainerLow = Color.White,
        surfaceContainer = Color(0xFFF9F8F4),
        surfaceContainerHigh = Color(0xFFECE9E1),
        surfaceContainerHighest = Color(0xFFE3DFD5),
        outlineVariant = Color(0xFFE3DFD5),
        outline = Color(0xFF6D6A78),
        error = Color(0xFFB53D22),
        onError = Color.White,
        errorContainer = Color(0xFFFBE6DF),
        onErrorContainer = Color(0xFF8F2E18),
    )
internal val CairnDarkColors =
    darkColorScheme(
        primary = Color(0xFF8DB8E8),
        onPrimary = Color(0xFF0D0D12),
        primaryContainer = Color(0xFF21354C),
        onPrimaryContainer = Color(0xFFDCE6F2),
        inversePrimary = Color(0xFF14365A),
        secondary = Color(0xFF9D9BAB),
        onSecondary = Color(0xFF0D0D12),
        secondaryContainer = Color(0xFF21354C),
        onSecondaryContainer = Color(0xFFF3F2F7),
        tertiary = Color(0xFF9898A5),
        onTertiary = Color(0xFF0D0D12),
        tertiaryContainer = Color(0xFF22222B),
        onTertiaryContainer = Color(0xFFF3F2F7),
        background = Color(0xFF0D0D12),
        onBackground = Color(0xFFF3F2F7),
        surface = Color(0xFF17171E),
        onSurface = Color(0xFFF3F2F7),
        surfaceVariant = Color(0xFF22222B),
        onSurfaceVariant = Color(0xFF9D9BAB),
        surfaceTint = Color(0xFF8DB8E8),
        inverseSurface = Color(0xFFF3F2F7),
        inverseOnSurface = Color(0xFF16151D),
        surfaceBright = Color(0xFF22222B),
        surfaceDim = Color(0xFF0D0D12),
        surfaceContainerLowest = Color(0xFF0D0D12),
        surfaceContainerLow = Color(0xFF17171E),
        surfaceContainer = Color(0xFF131319),
        surfaceContainerHigh = Color(0xFF22222B),
        surfaceContainerHighest = Color(0xFF2A2A34),
        outlineVariant = Color(0xFF2A2A34),
        outline = Color(0xFF9D9BAB),
        error = Color(0xFFFF8A70),
        onError = Color(0xFF2B130D),
        errorContainer = Color(0xFF38211C),
        onErrorContainer = Color(0xFFFFB4A3),
    )

/** Colours outside Material's roles: attention (coral), success and the ink surfaces. */
@androidx.compose.runtime.Immutable
internal data class CairnSignal(
    val attention: Color,
    val attentionSoft: Color,
    val success: Color,
    /** Low remaining usage, before it runs out. */
    val warning: Color,
    val warningSoft: Color,
    val ink: Color,
    val onInk: Color,
    val dock: Color,
    val onDock: Color,
)

internal val LightSignal =
    CairnSignal(
        attention = Color(0xFFE2512F),
        attentionSoft = Color(0xFFFBE6DF),
        success = Color(0xFF1E8A57),
        warning = Color(0xFF8A6412),
        warningSoft = Color(0xFFFFF0B5),
        ink = Color(0xFF16151D),
        onInk = Color(0xFFF4F2EC),
        dock = Color(0xFF16151D),
        onDock = Color(0xFFBDBAC7),
    )
internal val DarkSignal =
    CairnSignal(
        attention = Color(0xFFFF7B5E),
        attentionSoft = Color(0xFF38211C),
        success = Color(0xFF4FDB98),
        warning = Color(0xFFE5B94F),
        warningSoft = Color(0xFF403722),
        ink = Color(0xFFF3F2F7),
        onInk = Color(0xFF0D0D12),
        dock = Color(0xFF1E1E26),
        onDock = Color(0xFF9D9BAB),
    )

internal val LocalCairnSignal = androidx.compose.runtime.staticCompositionLocalOf { LightSignal }

internal val signal: CairnSignal
    @Composable get() = LocalCairnSignal.current

@Composable
fun cairnDarkTheme(preference: String): Boolean =
    when (preference) {
        "light" -> false
        "dark" -> true
        else -> isSystemInDarkTheme()
    }

@Composable
fun CairnTheme(preference: String = "system", content: @Composable () -> Unit) {
    val dark = cairnDarkTheme(preference)
    MaterialTheme(
        colorScheme = if (dark) CairnDarkColors else CairnLightColors,
        typography = CairnTypography,
    ) {
        androidx.compose.runtime.CompositionLocalProvider(
            LocalCairnSignal provides if (dark) DarkSignal else LightSignal,
            content = content,
        )
    }
}
