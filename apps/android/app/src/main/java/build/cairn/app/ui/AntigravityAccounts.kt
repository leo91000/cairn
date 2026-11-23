package build.cairn.app.ui

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import build.cairn.app.data.GOOGLE_ACCOUNT
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject

/*
 * Antigravity connections: a Gemini API key, Google Cloud credentials (sent as one JSON string in
 * the account's key) or a Google account signed in through Google's page after a terms warning.
 */

internal const val GEMINI_KEY = "gemini"
internal const val GOOGLE_CLOUD = "google-cloud"

/** Antigravity's terms forbid consumer sign-in from third-party apps; shown before every one. */
internal const val GOOGLE_SIGN_IN_WARNING =
    "Les conditions d’utilisation d’Antigravity de Google interdisent de se connecter avec un compte Google grand public depuis une application tierce comme Cairn ; Google peut suspendre le compte. Préférez une clé API Gemini ou des identifiants Google Cloud."

/** Credential files above this size are not service-account or authorized-user JSON. */
private const val CREDENTIALS_LIMIT = 64L * 1024

/** Why [credentials] cannot be sent, or null when it is a JSON object. */
internal fun googleCloudCredentialsError(credentials: String): String? {
    if (credentials.isBlank()) return null
    val parsed = runCatching { Json.parseToJsonElement(credentials) }.getOrNull()
    return when {
        parsed == null -> "Ce fichier n’est pas un JSON valide."
        parsed !is JsonObject -> "Le fichier d’identifiants doit contenir un objet JSON."
        else -> null
    }
}

/**
 * The account key for Google Cloud credentials: `{"credentials": {…}, "project": "…", "location":
 * "…"}`. A blank project or region is left out: the server reads the project from the file and
 * defaults the region to `global`.
 */
internal fun googleCloudKey(credentials: String, project: String, location: String): String {
    val parsed = Json.parseToJsonElement(credentials)
    require(parsed is JsonObject) { "Le fichier d’identifiants doit contenir un objet JSON." }
    return buildJsonObject {
        put("credentials", parsed)
        project.trim().takeIf { it.isNotEmpty() }?.let { put("project", JsonPrimitive(it)) }
        location.trim().takeIf { it.isNotEmpty() }?.let { put("location", JsonPrimitive(it)) }
    }
        .toString()
}

/** How an Antigravity account connects, the recommended choice first. */
internal val antigravityConnections =
    listOf(
        Triple(
            GEMINI_KEY,
            "Clé API Gemini",
            "Recommandé. Une clé créée dans Google AI Studio.",
        ),
        Triple(
            GOOGLE_CLOUD,
            "Google Cloud",
            "Fichier JSON de compte de service ou d’utilisateur autorisé, avec projet et région.",
        ),
        Triple(
            GOOGLE_ACCOUNT,
            "Compte Google",
            "Connexion sur la page de Google. Déconseillé par les conditions d’Antigravity.",
        ),
    )

@Composable
internal fun AntigravityConnectionChoice(
    selected: String,
    enabled: Boolean,
    choose: (String) -> Unit,
) {
    Column(Modifier.selectableGroup(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        antigravityConnections.forEach { (id, title, detail) ->
            val chosen = selected == id
            Surface(
                selected = chosen,
                onClick = { choose(id) },
                enabled = enabled,
                shape = RoundedCornerShape(14.dp),
                color =
                    if (chosen) MaterialTheme.colorScheme.primary.copy(alpha = 0.08f)
                    else MaterialTheme.colorScheme.surface,
                border =
                    BorderStroke(
                        if (chosen) 1.5.dp else 1.dp,
                        if (chosen) MaterialTheme.colorScheme.primary
                        else MaterialTheme.colorScheme.outlineVariant,
                    ),
                modifier = Modifier.fillMaxWidth().semantics { role = Role.RadioButton },
            ) {
                Row(
                    Modifier.padding(start = 4.dp, end = 14.dp, top = 8.dp, bottom = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    RadioButton(selected = chosen, onClick = null, enabled = enabled)
                    Spacer(Modifier.width(4.dp))
                    Column(Modifier.weight(1f)) {
                        Text(
                            title,
                            style = MaterialTheme.typography.titleSmall,
                            fontWeight = FontWeight.SemiBold,
                        )
                        Text(
                            detail,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        }
    }
}

/** Credentials JSON (pasted or read from a file), project and region. */
@Composable
internal fun GoogleCloudFields(
    credentials: String,
    project: String,
    location: String,
    enabled: Boolean,
    changeCredentials: (String) -> Unit,
    changeProject: (String) -> Unit,
    changeLocation: (String) -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var fileError by remember { mutableStateOf<String?>(null) }
    val picker =
        rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
            if (uri == null) return@rememberLauncherForActivityResult
            scope.launch {
                fileError =
                    try {
                        val (_, bytes) =
                            readDocument(context.contentResolver, uri, CREDENTIALS_LIMIT)
                        changeCredentials(bytes.decodeToString())
                        null
                    } catch (e: CancellationException) {
                        throw e
                    } catch (e: Exception) {
                        e.message ?: "Impossible de lire ce fichier."
                    }
            }
        }
    val error = fileError ?: googleCloudCredentialsError(credentials)
    OutlinedTextField(
        value = credentials,
        onValueChange = {
            fileError = null
            changeCredentials(it.take(CREDENTIALS_LIMIT.toInt()))
        },
        label = { Text("Identifiants JSON") },
        placeholder = { Text("{\"type\": \"service_account\", …}") },
        minLines = 4,
        maxLines = 8,
        isError = error != null,
        supportingText = {
            Text(error ?: "Collez le fichier JSON ou choisissez-le sur l’appareil.")
        },
        textStyle = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
        keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
        enabled = enabled,
        modifier = Modifier.fillMaxWidth(),
    )
    OutlinedButton(
        onClick = { picker.launch(arrayOf("application/json", "text/plain", "*/*")) },
        enabled = enabled,
    ) {
        Text("Choisir un fichier JSON")
    }
    OutlinedTextField(
        value = project,
        onValueChange = { changeProject(it.take(200)) },
        label = { Text("Projet") },
        placeholder = { Text("Lu depuis le fichier si absent") },
        singleLine = true,
        keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
        enabled = enabled,
        modifier = Modifier.fillMaxWidth(),
    )
    OutlinedTextField(
        value = location,
        onValueChange = { changeLocation(it.take(100)) },
        label = { Text("Région") },
        placeholder = { Text("global") },
        singleLine = true,
        keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
        enabled = enabled,
        modifier = Modifier.fillMaxWidth(),
    )
}

/** The terms warning, always visible while the Google account choice is selected. */
@Composable
internal fun GoogleSignInWarningNotice() {
    Surface(
        shape = RoundedCornerShape(14.dp),
        color = signal.attentionSoft,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(
            GOOGLE_SIGN_IN_WARNING,
            Modifier.padding(12.dp),
            style = MaterialTheme.typography.bodySmall,
            color = signal.attention,
        )
    }
}

/** Confirmation required before Cairn starts a Google account sign-in for Antigravity. */
@Composable
internal fun GoogleSignInWarningDialog(dismiss: () -> Unit, confirm: () -> Unit) {
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text("Se connecter avec un compte Google ?") },
        text = { Text(GOOGLE_SIGN_IN_WARNING) },
        confirmButton = {
            TextButton(onClick = confirm) {
                Text("J’accepte le risque, continuer", color = signal.attention)
            }
        },
        dismissButton = { TextButton(onClick = dismiss) { Text("Annuler") } },
    )
}
