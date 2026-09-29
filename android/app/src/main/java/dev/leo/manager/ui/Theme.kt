package dev.leo.manager.ui

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
import dev.leo.manager.R

private val LeoBody =
    FontFamily(
        Font(R.font.leo_body_400, FontWeight.Normal),
        Font(R.font.leo_body_500, FontWeight.Medium),
        Font(R.font.leo_body_600, FontWeight.SemiBold),
        Font(R.font.leo_body_700, FontWeight.Bold),
    )
private val LeoHeading =
    FontFamily(
        Font(R.font.leo_heading_600, FontWeight.SemiBold),
        Font(R.font.leo_heading_700, FontWeight.Bold),
    )

// The web's DM Sans / Manrope identity, with Android font scaling intact.
private val LeoTypography =
    Typography().let { base ->
        base.copy(
            displayLarge = base.displayLarge.copy(fontFamily = LeoHeading),
            displayMedium = base.displayMedium.copy(fontFamily = LeoHeading),
            displaySmall = base.displaySmall.copy(fontFamily = LeoHeading),
            // Screen titles: large, bold and slightly tightened for character.
            headlineLarge =
                base.headlineLarge.copy(
                    fontFamily = LeoHeading,
                    fontSize = 32.sp,
                    lineHeight = 36.sp,
                    fontWeight = FontWeight.Bold,
                    letterSpacing = (-0.8).sp,
                ),
            headlineMedium =
                base.headlineMedium.copy(
                    fontFamily = LeoHeading,
                    fontSize = 26.sp,
                    lineHeight = 31.sp,
                    fontWeight = FontWeight.Bold,
                    letterSpacing = (-0.6).sp,
                ),
            headlineSmall =
                base.headlineSmall.copy(
                    fontFamily = LeoHeading,
                    fontSize = 24.sp,
                    lineHeight = 29.sp,
                    fontWeight = FontWeight.Bold,
                    letterSpacing = (-0.4).sp,
                ),
            titleLarge =
                base.titleLarge.copy(
                    fontFamily = LeoHeading,
                    fontSize = 20.sp,
                    lineHeight = 28.sp,
                    fontWeight = FontWeight.SemiBold,
                ),
            titleMedium =
                base.titleMedium.copy(
                    fontFamily = LeoHeading,
                    fontSize = 16.sp,
                    lineHeight = 22.sp,
                    fontWeight = FontWeight.SemiBold,
                ),
            titleSmall =
                base.titleSmall.copy(
                    fontFamily = LeoBody,
                    fontSize = 14.sp,
                    lineHeight = 20.sp,
                    fontWeight = FontWeight.SemiBold,
                ),
            bodyLarge =
                base.bodyLarge.copy(
                    fontFamily = LeoBody,
                    fontSize = 16.sp,
                    lineHeight = 24.sp,
                    letterSpacing = 0.sp,
                ),
            bodyMedium =
                base.bodyMedium.copy(
                    fontFamily = LeoBody,
                    fontSize = 14.sp,
                    lineHeight = 20.sp,
                    letterSpacing = 0.sp,
                ),
            bodySmall =
                base.bodySmall.copy(fontFamily = LeoBody, fontSize = 12.sp, lineHeight = 16.sp),
            labelLarge =
                base.labelLarge.copy(fontFamily = LeoBody, fontSize = 13.sp, lineHeight = 18.sp),
            labelMedium =
                base.labelMedium.copy(fontFamily = LeoBody, fontSize = 12.sp, lineHeight = 16.sp),
            labelSmall =
                base.labelSmall.copy(fontFamily = LeoBody, fontSize = 11.sp, lineHeight = 14.sp),
        )
    }

// "Signal" palette: warm paper and ink, with the Leo blue as the single accent colour for
// actions, selection and live work. Coral is reserved for things that need the user.
internal val LeoLightColors =
    lightColorScheme(
        primary = Color(0xFF4545EF),
        onPrimary = Color.White,
        primaryContainer = Color(0xFFE7E6FD),
        onPrimaryContainer = Color(0xFF2F2FC4),
        inversePrimary = Color(0xFF8C88FF),
        secondary = Color(0xFF6D6A78),
        onSecondary = Color.White,
        secondaryContainer = Color(0xFFE7E6FD),
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
        surfaceTint = Color(0xFF4545EF),
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
internal val LeoDarkColors =
    darkColorScheme(
        primary = Color(0xFF8C88FF),
        onPrimary = Color(0xFF0D0D12),
        primaryContainer = Color(0xFF26254A),
        onPrimaryContainer = Color(0xFFC9C7FF),
        inversePrimary = Color(0xFF4545EF),
        secondary = Color(0xFF9D9BAB),
        onSecondary = Color(0xFF0D0D12),
        secondaryContainer = Color(0xFF26254A),
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
        surfaceTint = Color(0xFF8C88FF),
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
internal data class LeoSignal(
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
    LeoSignal(
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
    LeoSignal(
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

internal val LocalLeoSignal = androidx.compose.runtime.staticCompositionLocalOf { LightSignal }

internal val signal: LeoSignal
    @Composable get() = LocalLeoSignal.current

@Composable
fun leoDarkTheme(preference: String): Boolean =
    when (preference) {
        "light" -> false
        "dark" -> true
        else -> isSystemInDarkTheme()
    }

@Composable
fun LeoTheme(preference: String = "system", content: @Composable () -> Unit) {
    val dark = leoDarkTheme(preference)
    MaterialTheme(
        colorScheme = if (dark) LeoDarkColors else LeoLightColors,
        typography = LeoTypography,
    ) {
        androidx.compose.runtime.CompositionLocalProvider(
            LocalLeoSignal provides if (dark) DarkSignal else LightSignal,
            content = content,
        )
    }
}
