@file:OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)

package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.*

/** Program preparation belongs to the package, independently of its instances. */
@Composable
internal fun PluginPrograms(vm: CairnViewModel, state: Workspace, plugin: String) {
    var programs by remember(plugin) { mutableStateOf<ProgramInstallations?>(null) }
    suspend fun load() {
        programs = vm.api.listPluginPrograms(ListPluginProgramsInput(plugin))
    }
    Poll("plugin-programs-$plugin", 5000) {
        if (!state.busy)
            try {
                load()
            } catch (e: Exception) {
                vm.report(e)
            }
    }
    fun act(program: ProgramInstallation, action: String) = vm.perform {
        api.preparePluginProgram(
            when (action) {
                "check" -> PluginProgramInput.A(plugin, program.name)
                "install" ->
                    PluginProgramInput.B(requireNotNull(program.digest), plugin, program.name)
                "rollback" -> PluginProgramInput.C(plugin, program.name)
                else -> error("Unknown program action")
            }
        )
        load()
    }
    SignalCard(Modifier.fillMaxWidth().testTag("plugin-programs")) {
        Text("Programmes installés", style = MaterialTheme.typography.titleSmall)
        if (programs == null) LinearProgressIndicator(Modifier.fillMaxWidth())
        programs?.programs.orEmpty().forEach { program ->
            Text(
                "${program.name} · " +
                    (program.installed?.let { "Version ${it.release.version}" }
                        ?: "Pas encore installé · préparé à la première utilisation"),
                Modifier.padding(top = 8.dp),
                style = MaterialTheme.typography.bodyMedium,
            )
            program.candidate?.let { release ->
                Note("Disponible : ${release.version}")
                Note(release.url)
                Note(
                    if (release.sha256 != null) "L’empreinte publiée par l’éditeur sera vérifiée."
                    else
                        "La provenance repose sur HTTPS ; l’empreinte sera enregistrée au téléchargement."
                )
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(enabled = !state.busy, onClick = { act(program, "check") }) {
                    Text("Vérifier ${program.name}")
                }
                program.candidate?.let { release ->
                    Button(enabled = !state.busy, onClick = { act(program, "install") }) {
                        Text("Installer ${program.name} ${release.version}")
                    }
                }
                program.previous?.let { previous ->
                    OutlinedButton(enabled = !state.busy, onClick = { act(program, "rollback") }) {
                        Text("Restaurer ${program.name} ${previous.release.version}")
                    }
                }
            }
        }
    }
}
