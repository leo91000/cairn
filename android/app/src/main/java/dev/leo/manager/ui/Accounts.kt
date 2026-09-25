package dev.leo.manager.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.leo.manager.data.*
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject

/*
 * Coding-agent accounts: one list for Codex and Claude Code, a detail sheet per account and one
 * sign-in sheet for every coding agent. Selection rules and statuses come from the server.
 */

internal data class CodingAgent(val provider: String, val vendor: String, val subscription: String)

internal val codingAgents =
    listOf(
        CodingAgent("codex", "OpenAI", "Compte ChatGPT"),
        CodingAgent("claude", "Anthropic", "Compte Claude"),
    )

internal fun codingAgent(provider: String) =
    codingAgents.find { it.provider == provider } ?: codingAgents.first()

/** Remaining percentage under which a window is running low. */
internal const val LowPercent = 10.0

/** A short status beside the account; a ready account needs none. */
internal fun accountStatusLabel(account: Account, now: Long = System.currentTimeMillis()): String? =
    when (account.status) {
        "signIn" -> "Connexion à terminer"
        "reconnect" -> "À reconnecter"
        "paused" -> "En pause"
        "unavailable" -> "Usage indisponible"
        "waiting" -> account.resetsAt?.let { "Reprise ${resetsIn(it, now)}" } ?: "En attente"
        "full" -> "Complet"
        "next" -> "Prochain"
        "low" -> "Faible"
        else -> null
    }

/** "dans 48 min", "dans 2 h 10", then the local weekday and time. */
internal fun resetsIn(
    seconds: Long,
    now: Long = System.currentTimeMillis(),
    zone: ZoneId = ZoneId.systemDefault(),
): String {
    val left = seconds * 1000 - now
    val minutes = if (left <= 0) 0 else (left + 59_999) / 60_000
    return when {
        minutes <= 0 -> "maintenant"
        minutes < 60 -> "dans $minutes min"
        minutes < 1440 -> "dans ${minutes / 60} h ${"%02d".format(minutes % 60)}"
        else ->
            DateTimeFormatter.ofPattern("EEE HH:mm", Locale.FRENCH)
                .format(Instant.ofEpochSecond(seconds).atZone(zone))
    }
}

/** "5 heures", "Semaine" or a model window such as "Semaine · Opus". */
internal fun windowLabel(window: AccountWindow): String {
    val minutes = window.durationMins
    if (window.models.isEmpty() && minutes != null)
        when {
            minutes == 10080 -> return "Semaine"
            minutes % 1440 == 0 -> return count(minutes / 1440, "jour")
            minutes % 60 == 0 -> return count(minutes / 60, "heure")
        }
    // Model windows keep the server's label, such as "Weekly · Opus".
    return window.label
        .replace("Weekly", "Semaine")
        .replace(Regex("(\\d+)-(hour|day|minute) window")) { match ->
            count(
                match.groupValues[1].toInt(),
                mapOf("hour" to "heure", "day" to "jour")
                    .getOrDefault(match.groupValues[2], "minute"),
            )
        }
        .replace("Usage window", "Fenêtre d’usage")
}

private fun count(n: Int, unit: String) = "$n $unit${if (n > 1) "s" else ""}"

@Composable
internal fun ProviderBrand(provider: String, size: Dp = 34.dp) {
    Box(
        Modifier.size(size)
            .clip(RoundedCornerShape(size * 0.3f))
            .background(if (provider == "claude") ClaudeTint else signal.ink),
        contentAlignment = Alignment.Center,
    ) {
        ProviderMark(provider, size * 0.5f, if (provider == "claude") Color.White else signal.onInk)
    }
}

@Composable
internal fun AccountBadge(account: Account, modifier: Modifier = Modifier) {
    val label = accountStatusLabel(account) ?: return
    val (container, content) =
        when (account.status) {
            "next" ->
                MaterialTheme.colorScheme.primaryContainer to MaterialTheme.colorScheme.primary
            "low" -> signal.warningSoft to signal.warning
            "signIn",
            "reconnect" -> signal.attentionSoft to signal.attention
            else ->
                MaterialTheme.colorScheme.surfaceVariant to
                    MaterialTheme.colorScheme.onSurfaceVariant
        }
    Row(
        modifier
            .clip(CircleShape)
            .background(container)
            .padding(horizontal = 9.dp, vertical = 3.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (account.status == "next") {
            Box(Modifier.size(6.dp).background(content, CircleShape))
            Spacer(Modifier.width(5.dp))
        }
        Text(
            label,
            style = MaterialTheme.typography.labelMedium,
            fontWeight = FontWeight.SemiBold,
            color = content,
            maxLines = 1,
        )
    }
}

@Composable
private fun UsageBar(remaining: Double, muted: Boolean, modifier: Modifier = Modifier) {
    val tint =
        when {
            muted -> MaterialTheme.colorScheme.outline.copy(alpha = 0.6f)
            remaining <= 0 -> signal.attention
            remaining < LowPercent -> signal.warning
            else -> MaterialTheme.colorScheme.primary
        }
    Box(
        modifier.height(4.dp).clip(CircleShape).background(MaterialTheme.colorScheme.surfaceVariant)
    ) {
        Box(
            Modifier.fillMaxWidth(
                    (remaining / 100).toFloat().coerceIn(if (remaining > 0) 0.02f else 0f, 1f)
                )
                .fillMaxHeight()
                .clip(CircleShape)
                .background(tint)
        )
    }
}

/** One account in its coding agent's card. */
@Composable
internal fun AccountRow(account: Account, open: () -> Unit) {
    val windows = account.usage?.general.orEmpty().take(2)
    val running = account.activeRunIds.size
    val muted = account.status == "paused" || account.stale
    // The badge already says when the account needs signing in.
    val note =
        when {
            account.state != "ready" -> null
            account.usage?.error != null -> "Usage indisponible"
            windows.isEmpty() -> "Usage pas encore connu"
            else -> null
        }
    Surface(
        onClick = open,
        color = Color.Transparent,
        modifier =
            Modifier.fillMaxWidth().semantics(mergeDescendants = true) {
                contentDescription =
                    listOfNotNull(
                            account.name,
                            accountStatusLabel(account),
                            account.remainingPercent?.let { "${it.toInt()} % restants" },
                        )
                        .joinToString(", ")
            },
    ) {
        Column(
            Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (running > 0) WorkingAvatar(account.name, account.id, 36.dp)
                else AgentAvatar(account.name, account.id, 36.dp)
                Spacer(Modifier.width(12.dp))
                Column(Modifier.weight(1f)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            account.name,
                            Modifier.weight(1f, fill = false),
                            style = MaterialTheme.typography.titleSmall,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Spacer(Modifier.width(7.dp))
                        AccountBadge(account)
                    }
                    Text(
                        account.email ?: "Pas encore connecté",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                Spacer(Modifier.width(10.dp))
                Column(horizontalAlignment = Alignment.End) {
                    Text(
                        account.remainingPercent?.let { "${it.toInt()} %" } ?: "—",
                        style = MaterialTheme.typography.titleLarge,
                        fontWeight = FontWeight.Bold,
                        color =
                            if (muted) MaterialTheme.colorScheme.onSurfaceVariant
                            else MaterialTheme.colorScheme.onSurface,
                    )
                    val detail = if (running > 0) "$running en cours" else note
                    if (detail != null)
                        Text(
                            detail,
                            style = MaterialTheme.typography.bodySmall,
                            fontWeight =
                                if (running > 0) FontWeight.SemiBold else FontWeight.Normal,
                            color =
                                if (running > 0) MaterialTheme.colorScheme.primary
                                else MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                        )
                }
            }
            if (windows.isNotEmpty())
                Row(
                    Modifier.padding(start = 48.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    windows.forEach { UsageBar(it.remaining, muted, Modifier.weight(1f)) }
                }
        }
    }
}

/** The accounts of one coding agent. */
@Composable
internal fun CodingAgentCard(
    agent: CodingAgent,
    accounts: List<Account>,
    required: Boolean,
    add: () -> Unit,
    open: (Account) -> Unit,
) {
    val provider = agent.provider
    val running = accounts.sumOf { it.activeRunIds.size }
    SignalCard(
        Modifier.fillMaxWidth().testTag("accounts-$provider").semantics {
            contentDescription = "Comptes ${providerLabel(provider)}"
        },
        padding = PaddingValues(0.dp),
    ) {
        Row(
            Modifier.padding(start = 16.dp, end = 4.dp, top = 12.dp, bottom = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            ProviderBrand(provider)
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(providerLabel(provider), style = MaterialTheme.typography.titleMedium)
                Text(
                    "${agent.vendor} · ${accounts.size} compte${if (accounts.size > 1) "s" else ""}" +
                        if (running > 0) " · $running en cours" else "",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            RoundAction(
                "Ajouter un compte ${providerLabel(provider)}",
                LeoIcons.Plus,
                size = 40.dp,
                onClick = add,
            )
        }
        accounts.forEach { account ->
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            AccountRow(account) { open(account) }
        }
        if (accounts.isEmpty()) {
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            Text(
                "Aucun compte ${providerLabel(provider)}." +
                    if (required)
                        " Les exécutions ${providerLabel(provider)} attendent que vous en ajoutiez un."
                    else "",
                Modifier.padding(16.dp),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/** Usage left as concentric rings: the short window inside, the long one outside. */
@Composable
internal fun UsageRing(
    windows: List<AccountWindow>,
    remaining: Double?,
    muted: Boolean,
    size: Dp = 112.dp,
) {
    val primary = MaterialTheme.colorScheme.primary
    val track = MaterialTheme.colorScheme.surfaceVariant
    val outline = MaterialTheme.colorScheme.outline
    val warning = signal.warning
    Box(
        Modifier.size(size).clearAndSetSemantics {
            contentDescription = remaining?.let { "${it.toInt()} % restants" } ?: "Usage inconnu"
        },
        contentAlignment = Alignment.Center,
    ) {
        Canvas(Modifier.fillMaxSize()) {
            val stroke = this.size.minDimension / 11
            windows.take(2).forEachIndexed { index, window ->
                val inset =
                    stroke / 2 + if (windows.size > 1 && index == 0) stroke + 3.dp.toPx() else 0f
                val topLeft = Offset(inset, inset)
                val arc = Size(this.size.width - inset * 2, this.size.height - inset * 2)
                drawArc(track, 0f, 360f, false, topLeft, arc, style = Stroke(stroke))
                val color =
                    when {
                        muted -> outline.copy(alpha = 0.6f)
                        window.remaining < LowPercent -> warning
                        index == 1 || windows.size == 1 -> primary.copy(alpha = 0.5f)
                        else -> primary
                    }
                drawArc(
                    color,
                    -90f,
                    (360 * window.remaining / 100).toFloat(),
                    false,
                    topLeft,
                    arc,
                    style = Stroke(stroke, cap = StrokeCap.Round),
                )
            }
        }
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Text(
                remaining?.let { "${it.toInt()} %" } ?: "—",
                style = MaterialTheme.typography.headlineSmall,
                fontSize = (size.value * 0.19f).sp,
            )
            Text(
                "restant",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/** Everything about one account: its usage windows, what runs on it and its settings. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun AccountSheet(
    vm: LeoViewModel,
    state: Workspace,
    account: Account,
    openRun: (String) -> Unit,
    reconnect: () -> Unit,
    changed: suspend () -> Unit,
    close: () -> Unit,
) {
    var name by remember(account.id) { mutableStateOf(account.name) }
    var removing by remember { mutableStateOf(false) }
    var runs by remember { mutableStateOf<List<Run>>(emptyList()) }
    LaunchedEffect(account.activeRunIds) {
        val active =
            if (account.activeRunIds.isEmpty()) emptyList()
            else
                runCatching { vm.api.get<List<Run>>("/runs?status=running&limit=100") }
                    .getOrDefault(emptyList())
        withContext(Dispatchers.Main) { runs = active }
    }
    fun update(vararg fields: Pair<String, JsonPrimitive>) = vm.perform {
        api.request(
            "PATCH",
            "/accounts/${segment(account.id)}",
            buildJsonObject { fields.forEach { (key, value) -> put(key, value) } },
        )
        changed()
    }
    val general = account.usage?.general.orEmpty()
    val others = account.usage?.windows.orEmpty().filter { it.models.isNotEmpty() }
    val muted = account.status == "paused" || account.stale
    ModalBottomSheet(
        onDismissRequest = close,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = MaterialTheme.colorScheme.surface,
    ) {
        Column(
            Modifier.fillMaxWidth()
                .testTag("account-sheet")
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp)
                .padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (account.activeRunIds.isNotEmpty())
                    WorkingAvatar(account.name, account.id, 48.dp)
                else AgentAvatar(account.name, account.id, 48.dp)
                Spacer(Modifier.width(14.dp))
                Column(Modifier.weight(1f)) {
                    Text(
                        account.name,
                        style = MaterialTheme.typography.headlineSmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        ProviderMark(account.provider, 13.dp)
                        Spacer(Modifier.width(5.dp))
                        Text(
                            listOfNotNull(
                                    providerLabel(account.provider),
                                    account.email,
                                    account.plan,
                                )
                                .joinToString(" · "),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
                AccountBadge(account)
            }
            if (account.state == "error")
                Text(
                    account.error.ifBlank { "Reconnectez ce compte pour continuer à l’utiliser." },
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
            Eyebrow("Usage restant")
            if (general.isNotEmpty()) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    UsageRing(general, account.remainingPercent, muted)
                    Spacer(Modifier.width(18.dp))
                    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                        general.forEachIndexed { index, window ->
                            Legend(
                                if (index == 0) MaterialTheme.colorScheme.primary
                                else MaterialTheme.colorScheme.primary.copy(alpha = 0.5f),
                                "${windowLabel(window)} · ${window.remaining.toInt()} %",
                                window.resetsAt?.let { "Reprise ${resetsIn(it)}" },
                            )
                        }
                        account.usage
                            ?.resets
                            ?.takeIf { it.available > 0 }
                            ?.let {
                                Legend(
                                    MaterialTheme.colorScheme.outline,
                                    "${it.available} réinitialisation${if (it.available > 1) "s" else ""} en réserve",
                                    "Utilisées à 2 % restant",
                                )
                            }
                    }
                }
            } else
                Text(
                    when {
                        account.state == "pending" ->
                            "Terminez la connexion pour voir l’usage de ce compte."
                        account.usage?.error != null -> account.usage.error
                        account.status == "unavailable" ->
                            "L’usage apparaît après la première vérification, sous une minute."
                        else ->
                            "${providerLabel(account.provider)} n’a pas encore indiqué son usage. Les exécutions peuvent utiliser ce compte."
                    },
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            others.forEach { window ->
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Row {
                        Text(
                            windowLabel(window),
                            Modifier.weight(1f),
                            style = MaterialTheme.typography.bodySmall,
                        )
                        Text(
                            "${window.remaining.toInt()} %",
                            style = MaterialTheme.typography.bodySmall,
                            fontWeight = FontWeight.SemiBold,
                        )
                    }
                    UsageBar(window.remaining, muted, Modifier.fillMaxWidth())
                }
            }
            if (account.resetError.isNotBlank())
                Text(
                    account.resetError,
                    style = MaterialTheme.typography.bodySmall,
                    color = signal.warning,
                )
            else if (general.isNotEmpty() && account.stale)
                Text(
                    account.usage?.error
                        ?: "Ces valeurs datent. L’usage est vérifié à nouveau automatiquement.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )

            Eyebrow("En cours · ${account.activeRunIds.size} sur ${account.maxConcurrentRuns}")
            if (account.activeRunIds.isEmpty())
                Text(
                    "Rien ne tourne sur ce compte.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            // Other work, such as chat titles, can hold a slot without being a run.
            runs
                .filter { it.id in account.activeRunIds }
                .forEach { run ->
                    Surface(
                        onClick = {
                            close()
                            openRun(run.id)
                        },
                        shape = RoundedCornerShape(14.dp),
                        color = MaterialTheme.colorScheme.surfaceContainer,
                    ) {
                        Row(
                            Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            WorkingAvatar(run.title, run.id, 24.dp)
                            Spacer(Modifier.width(10.dp))
                            Text(
                                run.title,
                                Modifier.weight(1f),
                                style = MaterialTheme.typography.bodyMedium,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            Icon(
                                LeoIcons.Right,
                                null,
                                Modifier.size(16.dp),
                                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }

            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            OutlinedTextField(
                value = name,
                onValueChange = { name = it.take(100) },
                label = { Text("Nom") },
                singleLine = true,
                enabled = !state.busy,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                keyboardActions =
                    KeyboardActions(
                        onDone = {
                            if (name.isNotBlank() && name.trim() != account.name)
                                update("name" to JsonPrimitive(name.trim()))
                        }
                    ),
                modifier = Modifier.fillMaxWidth(),
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("Exécutions parallèles", style = MaterialTheme.typography.bodyMedium)
                    Text(
                        "Réduire laisse finir les exécutions en cours",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Stepper(account.maxConcurrentRuns, !state.busy) {
                    update("maxConcurrentRuns" to JsonPrimitive(it))
                }
            }
            Toggle("Utiliser pour les nouvelles exécutions", account.enabled) { enabled ->
                if (!state.busy && account.state != "pending")
                    update("enabled" to JsonPrimitive(enabled))
            }
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                SignalButton(
                    if (account.state == "pending") "Se connecter" else "Reconnecter",
                    icon = LeoIcons.Retry,
                    container = MaterialTheme.colorScheme.surface,
                    content = MaterialTheme.colorScheme.onSurface,
                    border = true,
                    height = 40.dp,
                    enabled = !state.busy && account.activeRunIds.isEmpty(),
                    onClick = reconnect,
                )
                Spacer(Modifier.weight(1f))
                TextButton(
                    onClick = {
                        vm.clearMessage()
                        removing = true
                    },
                    enabled = !state.busy && account.activeRunIds.isEmpty(),
                ) {
                    Text("Retirer", color = signal.attention)
                }
            }
        }
    }
    if (removing)
        Confirm(
            "Retirer ${account.name} ?",
            "Ses identifiants sont supprimés du serveur. Les exécutions ${providerLabel(account.provider)} utiliseront vos autres comptes.",
            state.busy,
            state.error,
            { removing = false },
        ) {
            vm.perform {
                api.request("DELETE", "/accounts/${segment(account.id)}")
                removing = false
                changed()
                close()
            }
        }
}

@Composable
private fun Legend(color: Color, title: String, detail: String?) {
    Row {
        Box(Modifier.padding(top = 6.dp).size(8.dp).background(color, CircleShape))
        Spacer(Modifier.width(8.dp))
        Column {
            Text(
                title,
                style = MaterialTheme.typography.bodySmall,
                fontWeight = FontWeight.SemiBold,
            )
            if (detail != null)
                Text(
                    detail,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
        }
    }
}

@Composable
private fun Stepper(value: Int, enabled: Boolean, change: (Int) -> Unit) {
    Row(
        Modifier.clip(CircleShape).background(MaterialTheme.colorScheme.surfaceContainer),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        TextButton(
            onClick = { change(value - 1) },
            enabled = enabled && value > 1,
            modifier = Modifier.semantics { contentDescription = "Moins d’exécutions parallèles" },
        ) {
            Text("−", style = MaterialTheme.typography.titleMedium)
        }
        Text(
            value.toString(),
            Modifier.widthIn(min = 24.dp),
            style = MaterialTheme.typography.titleMedium,
            textAlign = TextAlign.Center,
        )
        TextButton(
            onClick = { change(value + 1) },
            enabled = enabled,
            modifier = Modifier.semantics { contentDescription = "Plus d’exécutions parallèles" },
        ) {
            Text("+", style = MaterialTheme.typography.titleMedium)
        }
    }
}

/**
 * Adds an account or signs one in again, with the same steps for every coding agent: the official
 * page opens in the browser and this sheet follows until the account is verified.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun AccountSignInSheet(
    vm: LeoViewModel,
    state: Workspace,
    signIn: AccountSignIn?,
    initialProvider: String?,
    changed: suspend () -> Unit,
    close: () -> Unit,
) {
    var provider by remember { mutableStateOf(initialProvider ?: "codex") }
    var name by remember { mutableStateOf("") }
    // Authorization codes never enter saved instance state or preferences.
    var code by remember { mutableStateOf("") }
    var submitted by remember(signIn?.accountId, signIn?.state) { mutableStateOf(false) }
    val flow = signIn?.takeIf { it.state == "pending" || it.state == "failed" }
    fun dismiss() {
        if (flow != null)
            vm.perform {
                api.request("DELETE", "/accounts/sign-in")
                changed()
            }
        close()
    }
    ModalBottomSheet(
        onDismissRequest = ::dismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = MaterialTheme.colorScheme.surface,
    ) {
        Column(
            Modifier.fillMaxWidth()
                .testTag("account-sign-in")
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp)
                .padding(bottom = 28.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Column {
                Text(
                    when {
                        flow == null -> "Ajouter un compte"
                        flow.state == "failed" -> "Nouvel essai nécessaire"
                        else ->
                            "Connecter votre ${codingAgent(flow.provider).subscription.replaceFirstChar { it.lowercase() }}"
                    },
                    style = MaterialTheme.typography.headlineSmall,
                )
                if (flow == null)
                    Text(
                        "Votre mot de passe reste chez ${codingAgent(provider).vendor}.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
            }
            when {
                flow == null -> {
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(10.dp),
                        modifier = Modifier.height(IntrinsicSize.Min),
                    ) {
                        codingAgents.forEach { agent ->
                            ProviderTile(
                                agent.provider,
                                agent.subscription,
                                provider == agent.provider,
                                !state.busy,
                                Modifier.weight(1f).fillMaxHeight(),
                            ) {
                                provider = agent.provider
                            }
                        }
                    }
                    OutlinedTextField(
                        value = name,
                        onValueChange = { name = it.take(100) },
                        label = { Text("Nom") },
                        placeholder = { Text("ex. Travail") },
                        singleLine = true,
                        enabled = !state.busy,
                        modifier = Modifier.fillMaxWidth(),
                    )
                    SignalButton(
                        "Continuer vers la connexion",
                        icon = LeoIcons.External,
                        container = MaterialTheme.colorScheme.primary,
                        content = MaterialTheme.colorScheme.onPrimary,
                        expand = true,
                        enabled = !state.busy,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        vm.perform {
                            api.request(
                                "POST",
                                "/accounts",
                                body(
                                    "provider" to provider,
                                    "name" to
                                        name.trim().ifBlank { "Compte ${providerLabel(provider)}" },
                                ),
                            )
                            changed()
                        }
                    }
                }
                flow.state == "failed" -> {
                    Text(
                        flow.error ?: "La connexion n’a pas abouti.",
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                    )
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        SignalButton(
                            "Réessayer",
                            container = MaterialTheme.colorScheme.primary,
                            content = MaterialTheme.colorScheme.onPrimary,
                            enabled = !state.busy,
                        ) {
                            vm.perform {
                                api.request("POST", "/accounts/${segment(flow.accountId)}/sign-in")
                                changed()
                            }
                        }
                        TextButton(onClick = ::dismiss, enabled = !state.busy) { Text("Fermer") }
                    }
                }
                else -> {
                    val verifying = flow.phase == "verifying" || submitted
                    if (flow.provider == "codex") {
                        Step(
                            if (flow.code != null) StepState.DONE else StepState.CURRENT,
                            "1",
                            if (flow.code != null) "Code prêt" else "Obtention d’un code sécurisé…",
                        )
                        Step(
                            if (verifying) StepState.DONE
                            else if (flow.code != null) StepState.CURRENT else StepState.LATER,
                            "2",
                            "Saisissez ce code sur OpenAI",
                        ) {
                            if (flow.code != null && !verifying) {
                                Surface(
                                    shape = RoundedCornerShape(16.dp),
                                    color = MaterialTheme.colorScheme.surfaceContainer,
                                ) {
                                    Row(
                                        Modifier.fillMaxWidth()
                                            .padding(
                                                start = 16.dp,
                                                end = 4.dp,
                                                top = 6.dp,
                                                bottom = 6.dp,
                                            ),
                                        verticalAlignment = Alignment.CenterVertically,
                                    ) {
                                        Text(
                                            flow.code,
                                            Modifier.weight(1f).semantics {
                                                contentDescription =
                                                    "Code de vérification ${flow.code}"
                                            },
                                            style = MaterialTheme.typography.headlineSmall,
                                            fontFamily =
                                                androidx.compose.ui.text.font.FontFamily.Monospace,
                                            letterSpacing = 2.sp,
                                        )
                                        CopyButton("Copier", flow.code)
                                    }
                                }
                                flow.url?.let {
                                    ExternalButton("Ouvrir la page de vérification", it)
                                }
                            }
                        }
                    } else {
                        Step(
                            if (flow.url != null) StepState.DONE else StepState.CURRENT,
                            "1",
                            if (flow.url != null) "Connectez-vous sur la page d’Anthropic"
                            else "Préparation du lien de connexion…",
                        ) {
                            if (flow.url != null && !verifying)
                                ExternalButton("Ouvrir la connexion Claude", flow.url)
                        }
                        Step(
                            if (verifying) StepState.DONE
                            else if (flow.acceptsCode) StepState.CURRENT else StepState.LATER,
                            "2",
                            "Collez le code affiché par Anthropic",
                        ) {
                            if (flow.acceptsCode && !verifying) {
                                OutlinedTextField(
                                    value = code,
                                    onValueChange = { code = it },
                                    label = { Text("Code d’autorisation Claude") },
                                    singleLine = true,
                                    keyboardOptions = InputKeyboards.Password,
                                    visualTransformation = PasswordVisualTransformation(),
                                    enabled = !state.busy,
                                    modifier = Modifier.fillMaxWidth(),
                                )
                                SignalButton(
                                    "Terminer la connexion",
                                    container = MaterialTheme.colorScheme.primary,
                                    content = MaterialTheme.colorScheme.onPrimary,
                                    expand = true,
                                    enabled = !state.busy && code.isNotBlank(),
                                    modifier = Modifier.fillMaxWidth(),
                                ) {
                                    vm.perform {
                                        api.request(
                                            "POST",
                                            "/accounts/sign-in/code",
                                            body("code" to code.trim()),
                                        )
                                        code = ""
                                        submitted = true
                                        changed()
                                    }
                                }
                            }
                        }
                    }
                    Step(
                        if (verifying) StepState.CURRENT else StepState.LATER,
                        "3",
                        if (verifying) "Vérification du compte…"
                        else "Approuvez l’accès — cette fenêtre se termine seule",
                    )
                    TextButton(
                        onClick = ::dismiss,
                        enabled = !state.busy && flow.phase != "verifying",
                    ) {
                        Text("Annuler la connexion")
                    }
                }
            }
            state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        }
    }
}

private enum class StepState {
    DONE,
    CURRENT,
    LATER,
}

@Composable
private fun Step(
    step: StepState,
    number: String,
    title: String,
    content: @Composable ColumnScope.() -> Unit = {},
) {
    Row {
        Box(
            Modifier.size(28.dp)
                .clip(CircleShape)
                .background(
                    when (step) {
                        StepState.CURRENT -> MaterialTheme.colorScheme.primary
                        StepState.DONE -> MaterialTheme.colorScheme.primaryContainer
                        StepState.LATER -> MaterialTheme.colorScheme.surfaceVariant
                    }
                ),
            contentAlignment = Alignment.Center,
        ) {
            if (step == StepState.DONE)
                Icon(
                    LeoIcons.Check,
                    null,
                    Modifier.size(14.dp),
                    tint = MaterialTheme.colorScheme.primary,
                )
            else
                Text(
                    number,
                    style = MaterialTheme.typography.labelLarge,
                    fontWeight = FontWeight.Bold,
                    color =
                        if (step == StepState.CURRENT) MaterialTheme.colorScheme.onPrimary
                        else MaterialTheme.colorScheme.onSurfaceVariant,
                )
        }
        Spacer(Modifier.width(12.dp))
        Column(
            Modifier.weight(1f).padding(top = 4.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text(
                title,
                style = MaterialTheme.typography.titleSmall,
                color =
                    if (step == StepState.LATER) MaterialTheme.colorScheme.onSurfaceVariant
                    else MaterialTheme.colorScheme.onSurface,
            )
            content()
        }
    }
}
