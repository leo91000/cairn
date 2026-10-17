package build.cairn.app.ui

import androidx.compose.runtime.*
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import build.cairn.app.data.CairnViewModel
import build.cairn.app.data.Workspace
import build.cairn.app.diagnostics.Performance
import build.cairn.app.diagnostics.PerformanceUploader
import build.cairn.app.diagnostics.runPerformanceUploads

internal val LocalPerformanceUploadCadence = staticCompositionLocalOf {
    Performance.uploadCadence
}

@Composable
internal fun PerformanceUpload(vm: CairnViewModel, state: Workspace) {
    // Until the saved preference is loaded, fail closed on automatic sharing.
    val enabled by vm.telemetryEnabled.collectAsStateWithLifecycle(initialValue = false)
    val owner = LocalLifecycleOwner.current
    val cadence = LocalPerformanceUploadCadence.current
    val account =
        state.session.account?.id.takeIf { state.session.authenticated && !state.signingOut }
    LaunchedEffect(account) { Performance.store.clear() }
    LaunchedEffect(vm, owner, account, enabled, cadence) {
        if (account == null || !enabled) return@LaunchedEffect
        val uploader = PerformanceUploader(vm::uploadPerformance)
        owner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            runPerformanceUploads(cadence, uploader::upload)
        }
    }
}
