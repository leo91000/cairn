package build.cairn.app

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import build.cairn.app.data.CairnViewModel
import build.cairn.app.data.notificationTarget
import build.cairn.app.diagnostics.FrameMonitor
import build.cairn.app.ui.AppUpdatePrompt
import build.cairn.app.ui.CairnApp
import build.cairn.app.ui.CairnTheme
import build.cairn.app.ui.LocalAppUpdates
import build.cairn.app.ui.cairnDarkTheme
import build.cairn.app.update.UpdateViewModel

class MainActivity : ComponentActivity() {
    private val frameMonitor by lazy { FrameMonitor(window) }
    private var targetAccount by mutableStateOf("")
    private var targetChat by mutableStateOf("")
    private var targetCacheScope by mutableStateOf("")
    private var sharedUrl by mutableStateOf("")

    override fun onCreate(savedInstanceState: Bundle?) {
        setTheme(R.style.Theme_Cairn)
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        // After process death the system redelivers the launch intent, whose share may already
        // have been consumed; the share still waiting at that point is in the saved state.
        receive(intent, share = savedInstanceState == null)
        savedInstanceState?.getString(PENDING_SHARE)?.let { sharedUrl = it }
        setContent {
            val vm: CairnViewModel = viewModel()
            val preference by vm.theme.collectAsStateWithLifecycle(initialValue = "system")
            val dark = cairnDarkTheme(preference)
            SideEffect {
                val bars =
                    SystemBarStyle.auto(
                        android.graphics.Color.TRANSPARENT,
                        android.graphics.Color.TRANSPARENT,
                    ) {
                        dark
                    }
                enableEdgeToEdge(statusBarStyle = bars, navigationBarStyle = bars)
            }
            val updates: UpdateViewModel = viewModel()
            CairnTheme(preference) {
                CompositionLocalProvider(LocalAppUpdates provides updates) {
                    AppUpdatePrompt(updates)
                    CairnApp(
                        sharedUrl,
                        consumedShare = {
                            sharedUrl = ""
                            if (intent.action == Intent.ACTION_SEND)
                                intent.removeExtra(Intent.EXTRA_TEXT)
                        },
                        vm = vm,
                        targetChat = targetChat,
                        targetAccount = targetAccount,
                        targetCacheScope = targetCacheScope,
                        consumedTarget = { targetChat = "" },
                    )
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        if (intent.action == Intent.ACTION_SEND) setIntent(intent)
        receive(intent)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putString(PENDING_SHARE, sharedUrl)
    }

    override fun onStart() {
        super.onStart()
        frameMonitor.start()
    }

    override fun onStop() {
        frameMonitor.stop()
        super.onStop()
    }

    private fun receive(intent: Intent, share: Boolean = true) {
        notificationTarget(intent)?.let { target ->
            targetChat = target
            targetCacheScope = intent.getStringExtra("origin").orEmpty()
            targetAccount = intent.getStringExtra("account").orEmpty()
        }
        if (share && intent.action == Intent.ACTION_SEND && intent.type == "text/plain") {
            sharedUrl = intent.getStringExtra(Intent.EXTRA_TEXT).orEmpty()
        }
    }
}

private const val PENDING_SHARE = "pending-share"
