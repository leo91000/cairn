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
import build.cairn.app.ui.AppUpdatePrompt
import build.cairn.app.ui.CairnApp
import build.cairn.app.ui.CairnTheme
import build.cairn.app.ui.LocalAppUpdates
import build.cairn.app.ui.cairnDarkTheme
import build.cairn.app.update.UpdateViewModel

class MainActivity : ComponentActivity() {
    private var targetAccount by mutableStateOf("")
    private var targetChat by mutableStateOf("")
    private var targetCacheScope by mutableStateOf("")
    private var sharedUrl by mutableStateOf("")

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        receive(intent)
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
                        consumedShare = { sharedUrl = "" },
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
        receive(intent)
    }

    private fun receive(intent: Intent) {
        if (intent.action == "build.cairn.app.OPEN_CHAT") {
            targetChat =
                intent
                    .getStringExtra("chat")
                    .orEmpty()
                    .takeIf { runCatching { java.util.UUID.fromString(it) }.isSuccess }
                    .orEmpty()
            targetCacheScope = intent.getStringExtra("origin").orEmpty()
            targetAccount = intent.getStringExtra("account").orEmpty()
        }
        if (intent.action == Intent.ACTION_SEND && intent.type == "text/plain") {
            sharedUrl = intent.getStringExtra(Intent.EXTRA_TEXT).orEmpty()
        }
    }
}
