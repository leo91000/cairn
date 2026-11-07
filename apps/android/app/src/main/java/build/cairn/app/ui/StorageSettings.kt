package build.cairn.app.ui

import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import build.cairn.app.data.CairnViewModel
import build.cairn.contracts.CheckStorageInput
import build.cairn.contracts.GetStorageSettingsInput
import build.cairn.contracts.SaveStorageSettingsInput
import build.cairn.contracts.StorageSettings
import build.cairn.contracts.checkStorage
import build.cairn.contracts.getStorageSettings
import build.cairn.contracts.saveStorageSettings
import kotlinx.coroutines.launch

@Composable
internal fun StorageSettings(vm: CairnViewModel) {
    StorageSettings(
        load = { vm.api.getStorageSettings(GetStorageSettingsInput()) },
        save = { vm.api.saveStorageSettings(it) },
        check = { vm.api.checkStorage(CheckStorageInput()) },
    )
}

/** The default S3 storage for new disks. Credentials are sent once and never shown again. */
@Composable
internal fun StorageSettings(
    load: suspend () -> StorageSettings,
    save: suspend (SaveStorageSettingsInput) -> StorageSettings,
    check: suspend () -> Unit,
) {
    var storage by remember { mutableStateOf<StorageSettings?>(null) }
    var editing by rememberSaveable { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var notice by remember { mutableStateOf<String?>(null) }
    var bucket by rememberSaveable { mutableStateOf("") }
    var endpoint by rememberSaveable { mutableStateOf("") }
    var region by rememberSaveable { mutableStateOf("") }
    // Credentials stay in memory only, never in SavedState.
    var accessKeyId by remember { mutableStateOf("") }
    var secretAccessKey by remember { mutableStateOf("") }
    var privateBucketConfirmed by remember(endpoint, bucket) { mutableStateOf(false) }
    val r2 =
        runCatching { java.net.URI(endpoint) }
            .getOrNull()
            ?.let { it.scheme == "https" && it.host?.endsWith(".r2.cloudflarestorage.com") == true }
            ?: false
    val scope = rememberCoroutineScope()
    fun run(action: suspend () -> Unit) {
        scope.launch {
            busy = true
            error = null
            notice = null
            try {
                action()
            } catch (e: Exception) {
                error = e.message
            } finally {
                busy = false
            }
        }
    }
    LaunchedEffect(Unit) {
        try {
            storage = load()
        } catch (e: Exception) {
            error = e.message
        }
    }
    Panel {
        Text("Stockage des conversations", style = MaterialTheme.typography.titleLarge)
        val current = storage
        if (current == null) {
            if (error == null) LinearProgressIndicator()
        } else {
            Text(
                when {
                    current.integrated ->
                        "Vos disques utilisent le stockage S3 intégré à cette machine. Un S3 externe protège les nouveaux disques contre la perte de cette machine."
                    current.configured -> "Bucket S3 : ${current.bucket} · ${current.endpoint}"
                    else -> "Configurez S3 avant de démarrer une conversation."
                }
            )
            Text(
                "Changer le stockage par défaut s’applique aux nouveaux disques. Les disques existants gardent leur stockage d’origine ; laissez-le disponible."
            )
            if (current.environmentManaged)
                Text(
                    "Le stockage est configuré par des variables d’environnement du serveur. Retirez-les pour utiliser ces réglages."
                )
            OutlinedButton(
                onClick = { editing = !editing },
                enabled = !busy && !current.environmentManaged,
            ) {
                Text("Configurer un S3 externe")
            }
            if (current.configured)
                OutlinedButton(
                    onClick = {
                        run {
                            check()
                            notice = "Écriture, lecture et suppression réussies"
                        }
                    },
                    enabled = !busy,
                ) {
                    Text("Vérifier le stockage")
                }
            if (editing) {
                Field(
                    "Endpoint S3",
                    endpoint,
                    { endpoint = it },
                    enabled = !busy,
                    keyboardOptions = InputKeyboards.Uri,
                )
                Field(
                    "Bucket S3",
                    bucket,
                    { bucket = it },
                    enabled = !busy,
                    keyboardOptions = InputKeyboards.Literal,
                )
                Field(
                    "Région S3",
                    region,
                    { region = it },
                    enabled = !busy,
                    keyboardOptions = InputKeyboards.Literal,
                )
                SecretField("Clé d’accès S3", accessKeyId) { accessKeyId = it }
                SecretField("Clé secrète S3", secretAccessKey) { secretAccessKey = it }
                if (r2)
                    Toggle(
                        "Je confirme que les domaines publics R2 (y compris r2.dev) et les verrous de bucket sont désactivés dans Cloudflare. Cairn ne peut pas le vérifier via S3.",
                        privateBucketConfirmed,
                    ) {
                        privateBucketConfirmed = it
                    }
                Text(
                    "Utilisez un bucket privé dédié, sans règle de cycle de vie ni Object Lock. Cairn vérifie l’accès avant d’enregistrer."
                )
                Button(
                    onClick = {
                        run {
                            val input =
                                try {
                                    SaveStorageSettingsInput(
                                        accessKeyId = accessKeyId,
                                        bucket = bucket,
                                        endpoint = endpoint,
                                        privateBucketConfirmed = privateBucketConfirmed,
                                        region = region,
                                        secretAccessKey = secretAccessKey,
                                    )
                                } catch (_: IllegalArgumentException) {
                                    throw IllegalArgumentException(
                                        "Saisissez un endpoint HTTPS et un bucket en minuscules, chiffres, points ou tirets."
                                    )
                                }
                            storage = save(input)
                            accessKeyId = ""
                            secretAccessKey = ""
                            editing = false
                            notice = "Stockage enregistré"
                        }
                    },
                    enabled =
                        !busy &&
                            listOf(bucket, endpoint, region, accessKeyId, secretAccessKey).all {
                                it.isNotBlank()
                            } &&
                            (!r2 || privateBucketConfirmed),
                ) {
                    Text(if (busy) "Vérification du stockage…" else "Enregistrer le stockage S3")
                }
            }
        }
        error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        notice?.let { Text(it) }
    }
}
