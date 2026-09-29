package dev.leo.manager.ui

import android.graphics.BitmapFactory
import android.util.LruCache
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import dev.leo.manager.data.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

internal class PortraitLoader(private val api: LeoApi) {
    private val cache = LruCache<String, ImageBitmap>(48)
    private val lock = Mutex()

    suspend fun load(agent: Agent): ImageBitmap? = lock.withLock {
        val avatar = agent.avatar ?: return@withLock null
        val url = avatar.url ?: return@withLock null
        cache.get(url)?.let {
            return@withLock it
        }
        val bytes = api.agentPortrait(agent.id, avatar.revision)
        withContext(Dispatchers.Default) {
                BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
            }
            ?.also { cache.put(url, it) }
    }
}

internal data class PortraitContext(val agents: List<Agent>, val loader: PortraitLoader)

internal val LocalAgentPortraits = staticCompositionLocalOf<PortraitContext?> { null }

@Composable
internal fun rememberAgentPortraits(vm: LeoViewModel, state: Workspace): PortraitContext {
    val loader = remember(vm.api) { PortraitLoader(vm.api) }
    val pending = state.agents.any { it.avatar?.status == "generating" }
    LaunchedEffect(vm.api, pending) {
        while (pending) {
            delay(2000)
            try {
                vm.refreshAgentPortraits()
            } catch (e: Exception) {
                if (e is CancellationException) throw e
            }
        }
    }
    return remember(state.agents, loader) { PortraitContext(state.agents, loader) }
}

@Composable
internal fun AgentPortraitImage(id: String, fallback: @Composable () -> Unit) {
    val context = LocalAgentPortraits.current
    val agent = context?.agents?.find { it.id == id }
    val bitmap by
        produceState<ImageBitmap?>(null, context?.loader, agent?.avatar?.url) {
            value = null
            if (agent?.avatar?.url != null) {
                try {
                    value = context.loader.load(agent)
                } catch (e: Exception) {
                    if (e is CancellationException) throw e
                }
            }
        }
    val image = bitmap
    if (image == null) fallback()
    else Image(image, null, Modifier.fillMaxSize(), contentScale = ContentScale.Crop)
}

@Composable
internal fun AgentPortraitEditor(vm: LeoViewModel, state: Workspace, initial: Agent) {
    val agent = state.agents.find { it.id == initial.id } ?: initial
    var configured by remember { mutableStateOf(false) }
    val context = LocalContext.current
    LaunchedEffect(vm.api) {
        try {
            configured =
                wireJson
                    .parseToJsonElement(vm.api.request("GET", "/agent-avatars"))
                    .jsonObject["configured"]
                    ?.jsonPrimitive
                    ?.content == "true"
        } catch (e: Exception) {
            if (e is CancellationException) throw e
        }
    }
    val upload =
        rememberLauncherForActivityResult(ActivityResultContracts.GetContent()) { uri ->
            if (uri != null)
                vm.perform {
                    val bytes =
                        withContext(Dispatchers.IO) {
                            context.contentResolver.openInputStream(uri)?.use {
                                readPortraitBytes(it, 5 * 1024 * 1024)
                            } ?: error("Cette image n’est pas accessible.")
                        }
                    require(bytes.size <= 5 * 1024 * 1024) {
                        "Choisissez une image de moins de 5 Mo."
                    }
                    api.uploadAgentPortrait(agent.id, bytes)
                    refreshAgentPortraits()
                }
        }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Portrait", style = MaterialTheme.typography.titleMedium)
        AgentAvatar(agent.name, agent.id, 64.dp)
        Text(
            if (configured) "La génération utilise le quota de votre abonnement Codex."
            else "Connectez et activez un compte Codex dans Connexions pour générer des portraits.",
            style = MaterialTheme.typography.bodySmall,
        )
        if (agent.id.isBlank()) {
            Text(
                if (configured) "Un portrait illustré sera créé après l’enregistrement."
                else "Enregistrez l’agent pour importer un portrait."
            )
        } else {
            if (agent.avatar?.status == "generating")
                Text("Création du portrait… Vous pouvez continuer à travailler.")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (configured)
                    OutlinedButton(
                        enabled = !state.busy && agent.avatar?.status != "generating",
                        onClick = {
                            vm.perform {
                                api.request("POST", "/agents/${segment(agent.id)}/avatar/generate")
                                refreshAgentPortraits()
                            }
                        },
                    ) {
                        Text(if (agent.avatar?.url == null) "Générer" else "Régénérer")
                    }
                OutlinedButton(enabled = !state.busy, onClick = { upload.launch("image/*") }) {
                    Text("Importer")
                }
            }
            Text(
                "PNG, JPEG ou WebP · 5 Mo maximum. Le portrait est enregistré immédiatement.",
                style = MaterialTheme.typography.bodySmall,
            )
            if (agent.avatar?.status == "failed")
                Text(
                    "Le portrait n’a pas pu être généré. Réessayez ou importez une image.",
                    color = MaterialTheme.colorScheme.error,
                )
        }
    }
}
