package build.cairn.app.data

import android.app.Application
import android.os.Build
import android.os.Looper
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import build.cairn.app.diagnostics.PerfConnection
import build.cairn.app.diagnostics.PerfOperation
import build.cairn.app.diagnostics.PerfScreen
import build.cairn.app.diagnostics.Performance
import build.cairn.app.ui.ResourceSave
import java.io.File
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Opt-in fixture measurement; never reads the installed app's real account. */
@RunWith(AndroidJUnit4::class)
class RefreshMeasurementTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun measureEstablishedRoutes() = runBlocking {
        assumeTrue(
            InstrumentationRegistry.getArguments().getString("cairnRefreshMeasurement") == "true"
        )
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val configuration =
            wireJson
                .parseToJsonElement(File(context.filesDir, "direct-fixture.json").readText())
                .jsonObject
        val vault =
            object : SessionVault {
                override fun read(origin: String) =
                    configuration["cookie"]!!.jsonPrimitive.content + "; Path=/; HttpOnly"

                override fun write(origin: String, cookie: String?) = Unit
            }
        val models = ViewModelStore()
        val vm =
            withContext(Dispatchers.Main) {
                CairnViewModel(
                        context.applicationContext as Application,
                        vault,
                        configuration["origin"]!!.jsonPrimitive.content,
                    )
                    .also {
                        models.put("measurement", it)
                    }
            }
        compose.setContent {}
        val evidence = buildJsonArray {
            try {
                withTimeout(30000) { vm.state.first { it.ready && !it.busy } }
                assertTrue(vm.state.value.session.authenticated)
                val agent =
                    vm.api.send<JsonObject>("POST", "/agents", body("name" to "Native measurement"))
                val task =
                    vm.api.send<JsonObject>(
                        "POST",
                        "/tasks",
                        buildJsonObject {
                            put("name", "Native mission")
                            put("agentId", agent["id"]!!)
                            put("prompt", "Fixture only")
                            put("enabled", false)
                            put("worktree", false)
                        },
                    )
                val agentForm = wireJson.decodeFromJsonElement<Agent>(agent)
                val taskForm = wireJson.decodeFromJsonElement<Task>(task)
                for (route in listOf("relay", "direct")) {
                    vm.api.closeStreams()
                    if (route == "direct") {
                        vm.api.startDirect(context)
                        withTimeout(30000) {
                            do {
                                delay(100)
                                vm.api.request("GET", "/agents")
                            } while (vm.api.transport.route.value != "direct")
                        }
                    }
                    for ((kind, record, strategy) in
                        listOf(
                            Triple("agents", agent, "global"),
                            Triple("tasks", task, "global"),
                            Triple("tasks", task, "task-only-prototype"),
                        )) {
                        repeat(21) { index ->
                            withContext(Dispatchers.Main) {
                                Performance.mainThread = Looper.getMainLooper().thread
                                Performance.store.clear()
                                Performance.screen =
                                    if (kind == "tasks") PerfScreen.MISSIONS else PerfScreen.ATELIER
                                Performance.connection =
                                    if (route == "direct") PerfConnection.DIRECT
                                    else PerfConnection.RELAY
                                val id = record["id"]!!.jsonPrimitive.content
                                val saving = ResourceSave(id)
                                val editedName = "Native $kind $index"
                                val started = System.nanoTime()
                                val response =
                                    saving.save(
                                        vm.api,
                                        "/$kind",
                                        {
                                            if (kind == "agents")
                                                wireJson.encodeToJsonElement(
                                                    agentForm.copy(name = editedName)
                                                )
                                            else
                                                wireJson.encodeToJsonElement(
                                                    taskForm.copy(name = editedName)
                                                )
                                        },
                                    )
                                if (kind == "tasks") wireJson.decodeFromJsonElement<Task>(response)
                                val written = System.nanoTime()
                                if (strategy == "global") {
                                    vm.refresh()
                                    val published =
                                        if (kind == "agents")
                                            vm.state.value.agents.any {
                                                it.id == id && it.name == editedName
                                            }
                                        else
                                            vm.state.value.tasks.any {
                                                it.id == id && it.name == editedName
                                            }
                                    assertTrue(published)
                                } else
                                    coroutineScope {
                                        // Read-set experiment only: no new product invalidation
                                        // rules.
                                        val tasks = async { vm.api.get<List<Task>>("/tasks") }
                                        assertTrue(
                                            tasks.await().any {
                                                it.id == id && it.name == editedName
                                            }
                                        )
                                    }
                                val finished = System.nanoTime()
                                val snapshot = Performance.store.snapshot()
                                assertEquals(route, vm.api.transport.route.value)
                                // Resource calls above start on Main. Direct grant renewal runs
                                // on IO and may overlap a sample; retain it in the raw snapshot.
                                val writes =
                                    snapshot.operations
                                        .filter { it.operation == PerfOperation.API_WRITE }
                                        .sumOf { it.startedOnMainThread }
                                val reads =
                                    snapshot.operations
                                        .filter { it.operation == PerfOperation.API_READ }
                                        .sumOf { it.startedOnMainThread }
                                val allApiRequests =
                                    snapshot.operations
                                        .filter {
                                            it.operation == PerfOperation.API_READ ||
                                                it.operation == PerfOperation.API_WRITE
                                        }
                                        .sumOf { it.count }
                                val expectedReads = if (strategy == "global") 9L else 2L
                                assertEquals(1L, writes)
                                assertEquals(expectedReads, reads)
                                add(
                                    buildJsonObject {
                                        put("route", route)
                                        put("resource", kind)
                                        put("strategy", strategy)
                                        put("first", index == 0)
                                        put("applicationRequests", reads + writes)
                                        put(
                                            "backgroundApiRequests",
                                            allApiRequests - reads - writes,
                                        )
                                        put("writeMs", (written - started) / 1e6)
                                        put("totalMs", (finished - started) / 1e6)
                                        put("performance", wireJson.encodeToJsonElement(snapshot))
                                    }
                                )
                            }
                            delay(2000)
                        }
                    }
                }
            } finally {
                withContext(Dispatchers.Main) { models.clear() }
            }
        }
        File(context.filesDir, "refresh-measurements.json")
            .writeText(
                buildJsonObject {
                    put("sdk", Build.VERSION.SDK_INT)
                    put("device", Build.MODEL)
                    put("interSampleDelayMs", 2000)
                    put(
                        "workload",
                        buildJsonObject {
                            put("agents", vm.state.value.agents.size)
                            put("projects", vm.state.value.projects.size)
                            put("tasks", vm.state.value.tasks.size)
                            put("skills", vm.state.value.skills.size)
                            put("mcps", vm.state.value.mcps.size)
                            put(
                                "modelCatalogs",
                                buildJsonObject {
                                    vm.state.value.modelCatalogs.forEach { (provider, catalog) ->
                                        put(
                                            provider,
                                            buildJsonObject {
                                                put("models", catalog.models.size)
                                                put("stale", catalog.stale)
                                            },
                                        )
                                    }
                                },
                            )
                        },
                    )
                    put("samples", evidence)
                }
                    .toString()
            )
    }
}
