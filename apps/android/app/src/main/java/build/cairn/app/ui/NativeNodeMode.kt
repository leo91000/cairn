package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

@Serializable
internal data class NativeModeReview(val previewId: String, val preview: NativeModeImpact)

@Serializable
internal data class NativeModeImpact(
    val mode: String,
    val agents: List<NativeModeAgent> = emptyList(),
    val conversations: List<NativeModeConversation> = emptyList(),
    val tasks: List<NativeModeAgent> = emptyList(),
    val trustedAccounts: List<String> = emptyList(),
    val trustedPeople: List<NativeModePerson> = emptyList(),
)

@Serializable internal data class NativeModePerson(val id: String, val email: String? = null)

@Serializable internal data class NativeModeAgent(val id: String, val name: String = "")

@Serializable
internal data class NativeModeConversation(
    val id: String,
    val name: String? = null,
    val status: String = "",
)

internal const val NATIVE_ADMIN_WARNING =
    "Natif administrateur donne à toutes les conversations admises sudo sans mot de passe sur l’hôte réel. Root peut lire les fichiers et identifiants locaux, modifier Cairn, les services et processus, et contourner les budgets, baux et annulations coopératifs. Les droits Cairn distants restent inchangés. Le retour en protégé arrête les exécutions gérées et restreint les nouvelles ; il ne nettoie ni ne restaure la machine et ne crée pas une nouvelle frontière de confiance. Root peut déjouer son superviseur : l’arrêt confirmé reste coopératif."

@Composable
internal fun NativeNodeMode(vm: CairnViewModel, node: ExecutionNode, changed: suspend () -> Unit) {
    var review by remember(node.id) { mutableStateOf<NativeModeReview?>(null) }
    var busy by remember(node.id) { mutableStateOf(false) }
    var error by remember(node.id) { mutableStateOf<String?>(null) }
    val target =
        node.nativeModeProposal?.mode
            ?: if (node.nativeModeStatus == "failed" || node.nativeModeStatus == "pending")
                node.nativeModeRequest?.mode ?: "protected"
            else if (node.nativeModeRequest?.mode == "administrator") "protected"
            else "administrator"
    fun preview(mode: String = target) {
        busy = true
        error = null
        vm.perform {
            try {
                review =
                    api.send(
                        "POST",
                        "/nodes/${node.id}/native-mode",
                        buildJsonObject { put("mode", mode) },
                    )
            } catch (e: Exception) {
                error = e.message
            } finally {
                busy = false
            }
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            if (node.nativeModeObserved != null) nativeModeLabel(node.nativeModeObserved.mode)
            else if (node.nativeModeRequest != null) "Mode effectif non confirmé"
            else "Natif protégé",
            style = MaterialTheme.typography.titleSmall,
        )
        if (
            node.nativeModeObserved != null &&
                (node.nativeModeStatus in listOf("pending", "failed") ||
                    node.nativeModeObserved.status != "applied")
        )
            Text(
                "Dernier mode confirmé ci-dessus ; privilèges actuels non confirmés, nouveaux placements bloqués."
            )
        if (node.nativeModeStatus == "pending")
            Text(
                "Basculement en cours. Les nouveaux placements attendent la confirmation de l’arrêt et du mode."
            )
        if (node.nativeModeStatus == "failed" || node.nativeModeObserved?.status == "failed")
            Text(
                "Échec : nouveaux placements bloqués. ${node.nativeModeObserved?.error.orEmpty()}",
                color = MaterialTheme.colorScheme.error,
            )
        if (node.nativeModeProposal != null)
            Text("Un agent demande ${nativeModeLabel(target)}. Le propriétaire doit confirmer.")
        if (node.capabilities.nativeModeProtocol == 1)
            TextButton(
                onClick = { preview() },
                enabled = !busy && !node.revoked && node.nativeModeStatus != "pending",
            ) {
                Text(
                    if (node.nativeModeProposal != null) "Examiner la demande"
                    else if (node.nativeModeStatus == "failed") "Réessayer le basculement"
                    else "Passer en ${nativeModeLabel(target)}"
                )
            }
        else
            Text(
                "Mettez à jour le superviseur et installez sudo avec sa configuration sudoers.d standard pour changer de mode."
            )
        if (node.nativeModeStatus == "failed" && node.nativeModeRequest?.mode == "administrator")
            TextButton(onClick = { preview("protected") }, enabled = !busy) {
                Text("Revenir en Natif protégé")
            }
        error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    }
    review?.let { impact ->
        DetailSheet(
            "${node.name} : ${nativeModeLabel(impact.preview.mode)} ?",
            { if (!busy) review = null },
        ) {
            Text(NATIVE_ADMIN_WARNING)
            Text(
                "Toutes les exécutions gérées et leurs processus descendants seront arrêtés avant la relance. Les fichiers et l’historique restent conservés. Les messages concurrents, reprises, délégations et tâches planifiées attendent le mode confirmé et les autorisations actuelles."
            )
            Text(
                "Personnes de confiance : ${impact.preview.trustedPeople.map { it.email ?: it.id }.ifEmpty { impact.preview.trustedAccounts }.joinToString(", ")}"
            )
            Text("Agents qui recevront cet accès", style = MaterialTheme.typography.titleSmall)
            impact.preview.agents.forEach { Text(it.name) }
            Text(
                "Conversations concernées (${impact.preview.conversations.size})",
                style = MaterialTheme.typography.titleSmall,
            )
            impact.preview.conversations.forEach { Text("${it.name ?: it.id} · ${it.status}") }
            Text("Tâches planifiées", style = MaterialTheme.typography.titleSmall)
            impact.preview.tasks.forEach { Text(it.name) }
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            Button(
                onClick = {
                    busy = true
                    error = null
                    vm.perform {
                        try {
                            api.request(
                                "POST",
                                "/nodes/${node.id}/native-mode",
                                buildJsonObject {
                                    put("mode", impact.preview.mode)
                                    put("previewId", impact.previewId)
                                    put("confirm", true)
                                },
                            )
                            review = null
                            changed()
                        } catch (e: Exception) {
                            error = e.message
                        } finally {
                            busy = false
                        }
                    }
                },
                enabled = !busy,
                colors =
                    ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
            ) {
                Text("Confirmer l’accès et relancer les exécutions")
            }
        }
    }
}
