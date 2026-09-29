package dev.leo.manager

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
import dev.leo.manager.data.LeoViewModel
import dev.leo.manager.ui.AppUpdatePrompt
import dev.leo.manager.ui.LeoApp
import dev.leo.manager.ui.LeoTheme
import dev.leo.manager.ui.LocalAppUpdates
import dev.leo.manager.ui.leoDarkTheme
import dev.leo.manager.update.UpdateViewModel

class MainActivity : ComponentActivity() {
    private var targetChat by mutableStateOf("")
    private var targetOrigin by mutableStateOf("")
    private var sharedUrl by mutableStateOf("")

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        receive(intent)
        setContent {
            val vm: LeoViewModel = viewModel()
            val preference by vm.theme.collectAsStateWithLifecycle(initialValue = "system")
            val dark = leoDarkTheme(preference)
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
            LeoTheme(preference) {
                CompositionLocalProvider(LocalAppUpdates provides updates) {
                    AppUpdatePrompt(updates)
                    LeoApp(
                        sharedUrl,
                        consumedShare = { sharedUrl = "" },
                        vm = vm,
                        targetChat = targetChat,
                        targetOrigin = targetOrigin,
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
        if (intent.action == "dev.leo.manager.OPEN_CHAT") {
            targetChat =
                intent
                    .getStringExtra("chat")
                    .orEmpty()
                    .takeIf { runCatching { java.util.UUID.fromString(it) }.isSuccess }
                    .orEmpty()
            targetOrigin = intent.getStringExtra("origin").orEmpty()
        }
        if (intent.action == Intent.ACTION_SEND && intent.type == "text/plain") {
            sharedUrl = intent.getStringExtra(Intent.EXTRA_TEXT).orEmpty()
        }
    }
}
