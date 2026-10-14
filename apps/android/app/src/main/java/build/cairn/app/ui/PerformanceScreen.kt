package build.cairn.app.ui

import androidx.compose.runtime.compositionLocalOf
import build.cairn.app.diagnostics.PerfScreen

/** A pager's settled page can differ from the route that originally opened it. */
internal val LocalPerformanceScreen = compositionLocalOf<(PerfScreen?) -> Unit> { {} }
