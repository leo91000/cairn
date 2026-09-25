package dev.leo.manager.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import dev.leo.manager.data.*
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

abstract class ModelCatalogCases {
    @get:Rule val compose = createComposeRule()

    @Test fun openPickerLoadsNewCodexModelsAndRetryPreservesSelection() = exercise("codex")

    @Test fun openPickerRefreshesOnlyTheClaudeCatalog() = exercise("claude")

    private fun exercise(provider: String) {
        MockWebServer().use { server ->
            val upgraded = AtomicBoolean(false)
            val unavailable = AtomicBoolean(false)
            val requests = AtomicInteger()
            val otherRequests = AtomicInteger()
            val original = if (provider == "codex") "gpt-6-astra" else "sonnet"
            val additions =
                if (provider == "codex") listOf("gpt-6-sol", "gpt-6-luna")
                else listOf("opus", "haiku")
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val response =
                            when (request.path) {
                                "/api/session" -> """{"authenticated":true,"csrf":"fixture"}"""
                                "/api/$provider/models" -> {
                                    requests.incrementAndGet()
                                    if (unavailable.get())
                                        return MockResponse()
                                            .setResponseCode(503)
                                            .setBody("""{"error":"Temporarily unavailable"}""")
                                    val names =
                                        listOf(original) +
                                            if (upgraded.get()) additions else emptyList()
                                    """{"models":[${names.joinToString(",") { """{"model":"$it","displayName":"$it","defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high"}]}""" }}],"checkedAt":1}"""
                                }
                                "/api/codex/models",
                                "/api/claude/models" -> {
                                    otherRequests.incrementAndGet()
                                    "{}"
                                }
                                "/api/overview" -> "{}"
                                else -> "[]"
                            }
                        return MockResponse().setBody(response)
                    }
                }
            server.start()
            val vm =
                LeoViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    CatalogVault(),
                )
            var selection = original to "high"
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                var model by remember { mutableStateOf(original) }
                var effort by remember { mutableStateOf("high") }
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                val catalog = if (provider == "claude") state.claudeModels else state.models
                LeoTheme {
                    if (catalog.models.isNotEmpty())
                        Page {
                            ModelPicker(
                                catalog,
                                model,
                                effort,
                                refresh = { vm.refreshModels(provider) },
                            ) { m, r ->
                                model = m
                                effort = r
                                selection = m to r
                            }
                        }
                }
            }
            compose.waitUntil(15000) {
                compose.onAllNodesWithTag("model-picker").fetchSemanticsNodes().isNotEmpty()
            }
            val initialRequests = requests.get()
            val untouched = otherRequests.get()
            upgraded.set(true)
            compose.onNodeWithTag("model-picker").performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText(additions.last()).fetchSemanticsNodes().isNotEmpty()
            }
            assertTrue(requests.get() > initialRequests)
            assertEquals(untouched, otherRequests.get())
            assertEquals(original to "high", selection)

            unavailable.set(true)
            compose.onNodeWithTag("refresh-models").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Catalogue temporairement indisponible. Réessayez.")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText(additions.last()).assertExists()
            assertEquals(original to "high", selection)

            unavailable.set(false)
            compose.onNodeWithTag("refresh-models").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Catalogue temporairement indisponible. Réessayez.")
                    .fetchSemanticsNodes()
                    .isEmpty()
            }
            compose.onNodeWithText(additions.last()).performScrollTo().performClick()
            compose.runOnIdle { assertEquals(additions.last() to "", selection) }
            compose.onNodeWithText("Terminé").performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithTag("model-settings-sheet").fetchSemanticsNodes().isEmpty()
            }
            val beforeReopening = requests.get()
            compose.onNodeWithTag("model-picker").performScrollTo().performClick()
            compose.onNodeWithTag("model-settings-sheet").assertExists()
            compose.waitUntil(10000) { requests.get() > beforeReopening }
            compose.onNodeWithText("Terminé").performClick()
        }
    }
}

private class CatalogVault : SessionVault {
    private val values = mutableMapOf<String, String>()

    override fun read(origin: String) = values[origin]

    override fun write(origin: String, cookie: String?) {
        if (cookie == null) values.remove(origin) else values[origin] = cookie
    }
}
