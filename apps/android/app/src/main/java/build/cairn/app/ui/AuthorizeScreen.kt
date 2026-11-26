package build.cairn.app.ui

import android.os.Build
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewmodel.compose.viewModel
import build.cairn.app.data.*
import kotlinx.coroutines.flow.first
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull

private data class ScopePresentation(
    val title: String,
    val description: String,
    val icon: ImageVector,
    val elevated: Boolean,
)

private fun presentScope(scope: String): ScopePresentation =
    when (scope) {
        "read" ->
            ScopePresentation(
                title = "Lecture",
                description = "Lire les tâches, les profils, les skills et les résultats.",
                icon = AuthIcons.Eye,
                elevated = false,
            )
        "run" ->
            ScopePresentation(
                title = "Exécution complète",
                description = "Lancer et arrêter des tâches avec les accès complets du worker.",
                icon = AuthIcons.Terminal,
                elevated = true,
            )
        "manage" ->
            ScopePresentation(
                title = "Gestion",
                description =
                    "Créer, modifier et supprimer des données de l’installation, et configurer les accès des agents et les intégrations.",
                icon = AuthIcons.Pencil,
                elevated = false,
            )
        else ->
            ScopePresentation(
                title = scope,
                description = "Permission personnalisée demandée par ce client.",
                icon = AuthIcons.ShieldCheck,
                elevated = false,
            )
    }

private fun isLocalMcpRedirect(uri: String): Boolean {
    val parsed = uri.toHttpUrlOrNull() ?: return false
    return parsed.scheme == "http" && parsed.host in setOf("localhost", "127.0.0.1", "::1")
}

internal data class CompletedMcpConsent(
    val url: String,
    val accountId: String?,
    val preview: ConsentPreview,
    val installationId: String,
    val approved: Boolean,
    val redirect: String,
)

internal class McpConsentState : ViewModel() {
    // Keep the single-use callback in memory through rotation.
    var completed by mutableStateOf<CompletedMcpConsent?>(null)
}

private data class ReviewedMcpConsent(
    val url: String,
    val parameters: Map<String, String>,
    val preview: ConsentPreview,
)

@Composable
internal fun AuthorizeScreen(
    vm: CairnViewModel,
    state: Workspace,
    sharedUrl: String = "",
    consentState: McpConsentState = viewModel(),
    close: (() -> Unit)? = null,
) {
    val credentials = accountCredentials()
    val context = LocalContext.current
    var url by rememberSaveable(sharedUrl) { mutableStateOf(sharedUrl) }
    var review by remember { mutableStateOf<ReviewedMcpConsent?>(null) }
    var selectedInstallationId by rememberSaveable {
        mutableStateOf(state.installation?.id.orEmpty())
    }
    var confirmationRequired by remember { mutableStateOf(false) }
    var confirmationChallenge by remember { mutableStateOf<String?>(null) }
    var confirmationCode by remember { mutableStateOf("") }
    var editingUrl by remember { mutableStateOf(false) }
    val completed =
        consentState.completed?.takeIf {
            it.url == url && it.accountId == state.session.account?.id
        }

    fun loadPreview(targetUrl: String) {
        vm.perform {
            val (parsed, loaded) = previewMcpConsent(targetUrl)
            review = ReviewedMcpConsent(targetUrl, parsed, loaded)
            url = targetUrl
            editingUrl = false
            consentState.completed = null
            confirmationRequired = loaded.confirmationRequired
            confirmationChallenge = null
            confirmationCode = ""
            val choices = loaded.installations
            if (choices.none { it.id == selectedInstallationId }) {
                selectedInstallationId =
                    choices.firstOrNull { it.id == state.installation?.id }?.id
                        ?: choices.firstOrNull()?.id.orEmpty()
            }
        }
    }

    LaunchedEffect(sharedUrl) {
        if (sharedUrl.isNotBlank() && completed == null) {
            vm.state.first { !it.busy }
            loadPreview(url)
        }
    }

    fun decide(approved: Boolean, installationId: String) {
        val reviewed = review ?: return
        vm.perform {
            val result = recordMcpConsent(reviewed.parameters, approved, installationId)
            consentState.completed =
                CompletedMcpConsent(
                    reviewed.url,
                    state.session.account?.id,
                    reviewed.preview,
                    installationId,
                    approved,
                    result.redirect,
                )
        }
    }

    LaunchedEffect(state.error) {
        val message = state.error.orEmpty()
        if (
            message.contains("Confirm your identity", ignoreCase = true) ||
                message.contains("Confirmez votre identité", ignoreCase = true)
        ) {
            confirmationRequired = true
        }
    }

    AuthBackdrop {
        Column(
            Modifier.fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp, vertical = 16.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Column(
                Modifier.widthIn(max = 520.dp).fillMaxWidth(),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                if (close != null) {
                    TextButton(
                        onClick = close,
                        enabled = !state.busy,
                        modifier = Modifier.align(Alignment.End),
                    ) {
                        Text("Fermer")
                    }
                }
                state.session.account?.email?.let {
                    Text(
                        "Compte Cairn · $it",
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                state.error?.let { ErrorNotice(it, vm::clearMessage) }

                val details = completed?.preview ?: review?.preview
                if (details == null || editingUrl) {
                    AuthCard {
                        Heading(
                            "Autoriser un assistant",
                            "Vérifiez ce que le client pourra faire dans votre espace.",
                        )
                        Text(
                            "Partagez une page d’autorisation Cairn vers cette application depuis votre navigateur, ou collez son lien ici.",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        Field(
                            "Lien d’autorisation",
                            url,
                            { url = it },
                            enabled = !state.busy,
                            keyboardOptions = InputKeyboards.Uri,
                        )
                        Row(
                            Modifier.fillMaxWidth(),
                            horizontalArrangement = Arrangement.spacedBy(10.dp),
                        ) {
                            if (details != null) {
                                OutlinedButton(
                                    onClick = {
                                        url = checkNotNull(review).url
                                        editingUrl = false
                                    },
                                    enabled = !state.busy,
                                    shape = RoundedCornerShape(12.dp),
                                    modifier = Modifier.weight(1f).heightIn(min = 48.dp),
                                ) {
                                    Text("Annuler")
                                }
                            }
                            Button(
                                onClick = { loadPreview(url) },
                                enabled = url.isNotBlank() && !state.busy,
                                shape = RoundedCornerShape(12.dp),
                                modifier = Modifier.weight(1.4f).heightIn(min = 48.dp),
                            ) {
                                Text("Examiner la demande")
                            }
                        }
                    }
                } else if (completed != null) {
                    val approvedDecision = completed.approved
                    val installations = details.installations
                    val chosen = installations.firstOrNull { it.id == completed.installationId }

                    AuthCard(padding = PaddingValues(24.dp)) {
                        Column(
                            Modifier.fillMaxWidth(),
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.spacedBy(12.dp),
                        ) {
                            Box(
                                Modifier.size(56.dp)
                                    .clip(RoundedCornerShape(16.dp))
                                    .background(
                                        if (approvedDecision) signal.success.copy(alpha = 0.14f)
                                        else MaterialTheme.colorScheme.surfaceVariant
                                    ),
                                contentAlignment = Alignment.Center,
                            ) {
                                Icon(
                                    if (approvedDecision) CairnIcons.Check else CairnIcons.Close,
                                    null,
                                    Modifier.size(26.dp),
                                    tint =
                                        if (approvedDecision) signal.success
                                        else MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            Text(
                                if (approvedDecision) "Accès autorisé" else "Accès refusé",
                                style = MaterialTheme.typography.headlineSmall,
                                textAlign = TextAlign.Center,
                            )
                            Text(
                                "La décision est enregistrée. Retournez au client pour terminer la connexion.",
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                textAlign = TextAlign.Center,
                            )
                            Surface(
                                Modifier.fillMaxWidth(),
                                shape = RoundedCornerShape(12.dp),
                                color = MaterialTheme.colorScheme.surfaceContainer,
                                border =
                                    BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                            ) {
                                Column(
                                    Modifier.padding(14.dp),
                                    verticalArrangement = Arrangement.spacedBy(8.dp),
                                ) {
                                    SummaryRow("Client", details.client.client_name)
                                    if (chosen != null) SummaryRow("Installation", chosen.name)
                                    SummaryRow(
                                        "Permissions",
                                        details.scopes.joinToString(" · ") {
                                            presentScope(it).title
                                        },
                                    )
                                }
                            }
                            if (details.client.redirect_uri.isNotBlank()) {
                                RedirectDestination(
                                    details.client.redirect_uri,
                                    isLocalMcpRedirect(details.client.redirect_uri),
                                )
                            }
                            Button(
                                onClick = {
                                    runCatching { browse(context, completed.redirect) }
                                        .onFailure {
                                            vm.notify(
                                                "Impossible d’ouvrir le navigateur. Vérifiez qu’un navigateur est activé, puis réessayez."
                                            )
                                        }
                                },
                                shape = RoundedCornerShape(12.dp),
                                modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp),
                            ) {
                                Text("Retourner au client")
                            }
                            Text(
                                "Vous pouvez révoquer cet accès à tout moment dans Paramètres.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                textAlign = TextAlign.Center,
                            )
                        }
                    }
                } else {
                    val installations = details.installations
                    val chosen =
                        installations.firstOrNull { it.id == selectedInstallationId }
                            ?: installations.firstOrNull()
                    val isLocalRedirect = isLocalMcpRedirect(details.client.redirect_uri)

                    AuthCard {
                        // Connection bridge: [Client monogram] <-> [Cairn mark]
                        Row(
                            Modifier.fillMaxWidth(),
                            horizontalArrangement = Arrangement.Center,
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Box(
                                Modifier.size(48.dp)
                                    .clip(RoundedCornerShape(14.dp))
                                    .background(Color(0xFFD97757)),
                                contentAlignment = Alignment.Center,
                            ) {
                                Text(
                                    initial(details.client.client_name),
                                    style = MaterialTheme.typography.titleLarge,
                                    fontWeight = FontWeight.Bold,
                                    color = Color.White,
                                )
                            }
                            Spacer(Modifier.width(12.dp))
                            Box(
                                Modifier.size(28.dp)
                                    .clip(CircleShape)
                                    .background(MaterialTheme.colorScheme.surfaceContainer)
                                    .border(
                                        1.dp,
                                        MaterialTheme.colorScheme.outlineVariant,
                                        CircleShape,
                                    ),
                                contentAlignment = Alignment.Center,
                            ) {
                                Icon(
                                    AuthIcons.Link,
                                    null,
                                    Modifier.size(14.dp),
                                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            Spacer(Modifier.width(12.dp))
                            Box(
                                Modifier.size(48.dp)
                                    .clip(RoundedCornerShape(14.dp))
                                    .background(MaterialTheme.colorScheme.surfaceContainer)
                                    .border(
                                        1.dp,
                                        MaterialTheme.colorScheme.outlineVariant,
                                        RoundedCornerShape(14.dp),
                                    ),
                                contentAlignment = Alignment.Center,
                            ) {
                                CairnLogo()
                            }
                        }

                        Column(
                            Modifier.fillMaxWidth(),
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.spacedBy(4.dp),
                        ) {
                            Text(
                                "Autoriser « ${details.client.client_name} » ?",
                                style = MaterialTheme.typography.titleLarge,
                                textAlign = TextAlign.Center,
                            )
                            Text(
                                "Cet assistant demande l’accès MCP à votre espace Cairn.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                textAlign = TextAlign.Center,
                            )
                        }

                        // Amber Trust & Redirect card
                        Surface(
                            Modifier.fillMaxWidth(),
                            shape = RoundedCornerShape(14.dp),
                            color = signal.warningSoft,
                            border = BorderStroke(1.dp, signal.warning.copy(alpha = 0.35f)),
                        ) {
                            Column(
                                Modifier.padding(12.dp),
                                verticalArrangement = Arrangement.spacedBy(8.dp),
                            ) {
                                Row(
                                    verticalAlignment = Alignment.CenterVertically,
                                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                                ) {
                                    Icon(
                                        AuthIcons.Warning,
                                        null,
                                        Modifier.size(15.dp),
                                        tint = signal.warning,
                                    )
                                    Text(
                                        "Client non vérifié",
                                        style = MaterialTheme.typography.labelLarge,
                                        fontWeight = FontWeight.Bold,
                                        color = signal.warning,
                                    )
                                }
                                if (details.client.redirect_uri.isNotBlank()) {
                                    RedirectDestination(
                                        details.client.redirect_uri,
                                        isLocalRedirect,
                                    )
                                }
                                Text(
                                    "Ce nom est déclaré par le client et n’est pas vérifié par Cairn. Continuez uniquement si vous faites confiance à cette destination.",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = signal.warning,
                                )
                            }
                        }

                        // Installation selector
                        if (chosen != null) {
                            var menuOpen by remember { mutableStateOf(false) }
                            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                                Text(
                                    "Installation cible",
                                    style = MaterialTheme.typography.labelLarge,
                                    fontWeight = FontWeight.SemiBold,
                                )
                                Box {
                                    Surface(
                                        onClick = {
                                            if (installations.size > 1 && !state.busy)
                                                menuOpen = true
                                        },
                                        enabled = installations.size > 1 && !state.busy,
                                        shape = RoundedCornerShape(12.dp),
                                        color = MaterialTheme.colorScheme.surfaceContainer,
                                        border =
                                            BorderStroke(
                                                1.dp,
                                                MaterialTheme.colorScheme.outlineVariant,
                                            ),
                                        modifier = Modifier.fillMaxWidth(),
                                    ) {
                                        Row(
                                            Modifier.fillMaxWidth()
                                                .padding(horizontal = 12.dp, vertical = 10.dp),
                                            verticalAlignment = Alignment.CenterVertically,
                                        ) {
                                            Box(
                                                Modifier.size(8.dp)
                                                    .clip(CircleShape)
                                                    .background(
                                                        if (chosen.online) signal.success
                                                        else
                                                            MaterialTheme.colorScheme
                                                                .onSurfaceVariant
                                                    )
                                            )
                                            Spacer(Modifier.width(10.dp))
                                            Column(Modifier.weight(1f)) {
                                                Text(
                                                    chosen.name,
                                                    style = MaterialTheme.typography.titleSmall,
                                                    maxLines = 1,
                                                    overflow = TextOverflow.Ellipsis,
                                                )
                                                Text(
                                                    if (chosen.online) "En ligne" else "Hors ligne",
                                                    style = MaterialTheme.typography.labelSmall,
                                                    color =
                                                        MaterialTheme.colorScheme.onSurfaceVariant,
                                                )
                                            }
                                            if (installations.size > 1) {
                                                Icon(
                                                    CairnIcons.Down,
                                                    "Choisir une installation",
                                                    Modifier.size(18.dp),
                                                    tint =
                                                        MaterialTheme.colorScheme.onSurfaceVariant,
                                                )
                                            }
                                        }
                                    }
                                    DropdownMenu(menuOpen, { menuOpen = false }) {
                                        installations.forEach { item ->
                                            DropdownMenuItem(
                                                text = {
                                                    Column {
                                                        Text(item.name)
                                                        Text(
                                                            if (item.online) "En ligne"
                                                            else "Hors ligne",
                                                            style =
                                                                MaterialTheme.typography.labelSmall,
                                                            color =
                                                                MaterialTheme.colorScheme
                                                                    .onSurfaceVariant,
                                                        )
                                                    }
                                                },
                                                trailingIcon = {
                                                    if (item.id == chosen.id)
                                                        Icon(CairnIcons.Check, "Sélectionnée")
                                                },
                                                onClick = {
                                                    selectedInstallationId = item.id
                                                    menuOpen = false
                                                },
                                            )
                                        }
                                    }
                                }
                            }
                        } else {
                            Text(
                                "Ajoutez une installation dont vous êtes propriétaire pour accorder un accès.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }

                        // Tiered scope cards
                        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                            Text(
                                "Permissions demandées",
                                style = MaterialTheme.typography.labelLarge,
                                fontWeight = FontWeight.SemiBold,
                            )
                            Surface(
                                Modifier.fillMaxWidth(),
                                shape = RoundedCornerShape(14.dp),
                                color = MaterialTheme.colorScheme.surfaceContainer,
                                border =
                                    BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                            ) {
                                Column {
                                    details.scopes.forEachIndexed { index, scope ->
                                        val info = presentScope(scope)
                                        if (index > 0) {
                                            HorizontalDivider(
                                                color = MaterialTheme.colorScheme.outlineVariant
                                            )
                                        }
                                        val rowBackground =
                                            if (info.elevated)
                                                Brush.horizontalGradient(
                                                    listOf(
                                                        signal.attentionSoft.copy(alpha = 0.75f),
                                                        Color.Transparent,
                                                    )
                                                )
                                            else null
                                        Row(
                                            Modifier.fillMaxWidth()
                                                .then(
                                                    if (rowBackground != null)
                                                        Modifier.background(rowBackground)
                                                    else Modifier
                                                )
                                                .padding(horizontal = 12.dp, vertical = 11.dp),
                                            horizontalArrangement = Arrangement.spacedBy(10.dp),
                                            verticalAlignment = Alignment.Top,
                                        ) {
                                            Box(
                                                Modifier.size(30.dp)
                                                    .clip(RoundedCornerShape(9.dp))
                                                    .background(
                                                        if (info.elevated) signal.attentionSoft
                                                        else MaterialTheme.colorScheme.surface
                                                    )
                                                    .border(
                                                        1.dp,
                                                        if (info.elevated)
                                                            signal.attention.copy(alpha = 0.35f)
                                                        else
                                                            MaterialTheme.colorScheme
                                                                .outlineVariant,
                                                        RoundedCornerShape(9.dp),
                                                    ),
                                                contentAlignment = Alignment.Center,
                                            ) {
                                                Icon(
                                                    info.icon,
                                                    null,
                                                    Modifier.size(15.dp),
                                                    tint =
                                                        if (info.elevated) signal.attention
                                                        else MaterialTheme.colorScheme.primary,
                                                )
                                            }
                                            Column(Modifier.weight(1f)) {
                                                Row(
                                                    verticalAlignment = Alignment.CenterVertically,
                                                    horizontalArrangement =
                                                        Arrangement.spacedBy(8.dp),
                                                ) {
                                                    Text(
                                                        info.title,
                                                        style = MaterialTheme.typography.titleSmall,
                                                    )
                                                    if (info.elevated) {
                                                        Surface(
                                                            shape = CircleShape,
                                                            color = signal.attentionSoft,
                                                            border =
                                                                BorderStroke(
                                                                    1.dp,
                                                                    signal.attention.copy(
                                                                        alpha = 0.35f
                                                                    ),
                                                                ),
                                                        ) {
                                                            Text(
                                                                "Accès élevé",
                                                                Modifier.padding(
                                                                    horizontal = 7.dp,
                                                                    vertical = 2.dp,
                                                                ),
                                                                style =
                                                                    MaterialTheme.typography
                                                                        .labelSmall,
                                                                fontWeight = FontWeight.Bold,
                                                                color = signal.attention,
                                                            )
                                                        }
                                                    }
                                                }
                                                Text(
                                                    info.description,
                                                    style = MaterialTheme.typography.bodySmall,
                                                    color =
                                                        MaterialTheme.colorScheme.onSurfaceVariant,
                                                )
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Inline identity confirmation
                        if (confirmationRequired) {
                            Surface(
                                Modifier.fillMaxWidth(),
                                shape = RoundedCornerShape(14.dp),
                                color = MaterialTheme.colorScheme.surfaceContainer,
                                border =
                                    BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                            ) {
                                Column(
                                    Modifier.padding(12.dp),
                                    verticalArrangement = Arrangement.spacedBy(10.dp),
                                ) {
                                    Row(
                                        verticalAlignment = Alignment.CenterVertically,
                                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                                    ) {
                                        Icon(
                                            AuthIcons.ShieldCheck,
                                            null,
                                            Modifier.size(16.dp),
                                            tint = MaterialTheme.colorScheme.primary,
                                        )
                                        Text(
                                            "Confirmation d’identité requise",
                                            style = MaterialTheme.typography.titleSmall,
                                        )
                                    }
                                    Text(
                                        "Confirmez votre session par passkey ou code e-mail avant d’accorder cet accès.",
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                    if (Build.VERSION.SDK_INT >= 28) {
                                        FilledTonalButton(
                                            onClick = {
                                                vm.perform {
                                                    confirmAccountPasskey(credentials)
                                                    confirmationRequired = false
                                                    confirmationChallenge = null
                                                    confirmationCode = ""
                                                    notify("Identité confirmée.")
                                                }
                                            },
                                            enabled = !state.busy,
                                            shape = RoundedCornerShape(10.dp),
                                            modifier = Modifier.fillMaxWidth(),
                                        ) {
                                            SocialProviderIcon(AccountProvider.Passkey)
                                            Spacer(Modifier.width(8.dp))
                                            Text("Confirmer avec une passkey")
                                        }
                                    }
                                    OutlinedButton(
                                        onClick = {
                                            vm.perform {
                                                confirmationChallenge =
                                                    accountConfirmationCode().challenge
                                            }
                                        },
                                        enabled = !state.busy,
                                        shape = RoundedCornerShape(10.dp),
                                        modifier = Modifier.fillMaxWidth(),
                                    ) {
                                        Text(
                                            if (confirmationChallenge == null)
                                                "Recevoir un code par e-mail"
                                            else "Demander un code par e-mail"
                                        )
                                    }
                                    if (confirmationChallenge != null) {
                                        Text(
                                            "Utilisez le code du dernier e-mail. Cairn en envoie un nouveau après l’expiration du précédent.",
                                            style = MaterialTheme.typography.bodySmall,
                                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                        OtpCodeField(
                                            label = "Code de confirmation",
                                            value = confirmationCode,
                                            onValueChange = { confirmationCode = it },
                                            enabled = !state.busy,
                                        )
                                        Button(
                                            onClick = {
                                                vm.perform {
                                                    confirmAccountEmail(
                                                        checkNotNull(confirmationChallenge),
                                                        confirmationCode,
                                                    )
                                                    confirmationRequired = false
                                                    confirmationChallenge = null
                                                    confirmationCode = ""
                                                    notify("Identité confirmée.")
                                                }
                                            },
                                            enabled =
                                                !state.busy &&
                                                    confirmationCode.length == EMAIL_CODE_LENGTH,
                                            shape = RoundedCornerShape(10.dp),
                                            modifier = Modifier.fillMaxWidth(),
                                        ) {
                                            Text("Valider la confirmation")
                                        }
                                    }
                                }
                            }
                        } else {
                            Surface(
                                Modifier.fillMaxWidth(),
                                shape = RoundedCornerShape(11.dp),
                                color = signal.success.copy(alpha = 0.12f),
                                border = BorderStroke(1.dp, signal.success.copy(alpha = 0.28f)),
                            ) {
                                Row(
                                    Modifier.padding(horizontal = 12.dp, vertical = 9.dp),
                                    verticalAlignment = Alignment.CenterVertically,
                                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                                ) {
                                    Icon(
                                        AuthIcons.ShieldCheck,
                                        null,
                                        Modifier.size(16.dp),
                                        tint = signal.success,
                                    )
                                    Text(
                                        "Identité confirmée",
                                        style = MaterialTheme.typography.labelMedium,
                                        fontWeight = FontWeight.SemiBold,
                                        color = signal.success,
                                    )
                                }
                            }
                        }

                        // Primary / Secondary actions
                        val targetInstallationId = chosen?.id.orEmpty()
                        Row(
                            Modifier.fillMaxWidth(),
                            horizontalArrangement = Arrangement.spacedBy(10.dp),
                        ) {
                            OutlinedButton(
                                onClick = { decide(false, targetInstallationId) },
                                enabled = !state.busy,
                                shape = RoundedCornerShape(12.dp),
                                modifier = Modifier.weight(1f).heightIn(min = 48.dp),
                            ) {
                                Text("Refuser")
                            }
                            Button(
                                onClick = { decide(true, targetInstallationId) },
                                enabled =
                                    !state.busy &&
                                        !confirmationRequired &&
                                        targetInstallationId.isNotBlank(),
                                shape = RoundedCornerShape(12.dp),
                                modifier = Modifier.weight(1.35f).heightIn(min = 48.dp),
                            ) {
                                Text("Autoriser")
                            }
                        }

                        Row(
                            Modifier.fillMaxWidth(),
                            horizontalArrangement = Arrangement.SpaceBetween,
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(
                                "Révocable dans Paramètres.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            TextButton(onClick = { editingUrl = true }, enabled = !state.busy) {
                                Text("Changer de lien")
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun RedirectDestination(uri: String, isLocal: Boolean) {
    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(10.dp),
        color = MaterialTheme.colorScheme.surface,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column(
            Modifier.padding(horizontal = 10.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(5.dp),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                Icon(
                    if (isLocal) AuthIcons.Laptop else AuthIcons.Lock,
                    null,
                    Modifier.size(14.dp),
                    tint = if (isLocal) MaterialTheme.colorScheme.primary else signal.success,
                )
                Text("Destination", style = MaterialTheme.typography.labelMedium)
                Text(
                    if (isLocal) "App locale" else "HTTPS",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            SelectionContainer {
                Text(
                    uri,
                    style = MaterialTheme.typography.labelMedium,
                    fontFamily = FontFamily.Monospace,
                    fontWeight = FontWeight.SemiBold,
                )
            }
        }
    }
}

@Composable
private fun SummaryRow(label: String, value: String) {
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            label,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.width(12.dp))
        Text(
            value,
            style = MaterialTheme.typography.bodySmall,
            fontWeight = FontWeight.SemiBold,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}
