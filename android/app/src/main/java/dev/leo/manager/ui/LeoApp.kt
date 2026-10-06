@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
)

package dev.leo.manager.ui

import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleOut
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.compose.*
import dev.leo.manager.data.*

/** Top-level destinations reached from the dock; everything else is a pushed screen. */
private val topLevel = setOf("fil", "missions", "atelier")

/** Screens that take the whole height: no dock, no generic top bar. */
private fun focusedRoute(route: String) =
    route.startsWith("chat/") ||
        route.startsWith("new-chat") ||
        route.startsWith("run/") ||
        route == "search"

internal fun dockSelection(route: String): String =
    when {
        route in topLevel -> route
        route.startsWith("chat/") || route.startsWith("new-chat") || route == "search" -> "fil"
        route.startsWith("run/") -> "missions"
        else -> "atelier"
    }

@Composable
fun LeoApp(
    sharedUrl: String = "",
    consumedShare: () -> Unit = {},
    vm: LeoViewModel = viewModel(),
    targetChat: String = "",
    targetCacheScope: String = "",
    consumedTarget: () -> Unit = {},
) {
    val state by vm.state.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(state.notice) {
        state.notice?.let {
            snackbar.showSnackbar(it)
            vm.clearNotice()
        }
    }
    if (!state.ready) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CircularProgressIndicator()
        }
        return
    }
    if (state.restoringSession) {
        Page {
            Heading(
                "Connexion au service officiel",
                "Votre session est conservée sur cet appareil.",
            )
            Text(state.error.orEmpty())
            Button(onClick = { vm.perform { retrySession() } }, enabled = !state.busy) {
                Text("Réessayer")
            }
            if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
        }
        return
    }
    if (!state.session.authenticated) {
        LoginScreen(vm, state)
        return
    }
    Poll(state.session.account?.id, 30_000) {
        if (!state.busy) {
            try {
                vm.refreshInstallations()
            } catch (e: Exception) {
                vm.report(e)
            }
        }
    }

    if (state.installation == null) {
        val context = LocalContext.current
        Page {
            Heading(
                "Aucune installation",
                "Ajoutez ou revendiquez une installation depuis le service officiel.",
            )
            Text(state.session.account?.email.orEmpty())
            Text("Les installations de votre compte Leo apparaîtront ici.")
            Text(
                "Obtenez la commande d’installation dans le service officiel, ou associez une installation existante avec leo claim."
            )
            Button(onClick = { browse(context, state.origin.trimEnd('/') + "/claim") }) {
                Text("Ajouter une installation")
            }
            TextButton(onClick = { vm.perform { refreshInstallations() } }) { Text("Actualiser") }
            TextButton(onClick = { vm.perform { logout() } }) { Text("Se déconnecter") }
            state.error?.let { ErrorNotice(it, vm::clearMessage) }
        }
        return
    }
    val installation = checkNotNull(state.installation)
    Column(Modifier.fillMaxSize()) {
        var choosing by remember { mutableStateOf(false) }
        Box(Modifier.fillMaxWidth().statusBarsPadding().padding(horizontal = 12.dp)) {
            if (state.session.installations.size > 1)
                TextButton(
                    onClick = { choosing = true },
                    enabled = state.session.installations.size > 1 && !state.busy,
                    modifier =
                        Modifier.semantics { contentDescription = "Choisir une installation" },
                ) {
                    InstallationLabel(installation)
                }
            else
                InstallationLabel(
                    installation,
                    Modifier.padding(horizontal = 8.dp, vertical = 12.dp),
                )
            DropdownMenu(expanded = choosing, onDismissRequest = { choosing = false }) {
                state.session.installations.forEach { choice ->
                    DropdownMenuItem(
                        text = {
                            InstallationLabel(choice)
                        },
                        onClick = {
                            choosing = false
                            vm.perform { selectInstallation(choice.id) }
                        },
                    )
                }
            }
        }
        key(installation.id, installation.role, state.session.csrf) {
            Box(Modifier.weight(1f)) {
                if (!installation.online)
                    Page {
                        Heading(
                            "Installation hors ligne",
                            "Les exécutions continuent sur votre installation. Réessayez lorsqu’elle sera connectée.",
                        )
                        TextButton(onClick = { vm.perform { refreshInstallations() } }) {
                            Text("Actualiser")
                        }
                        TextButton(onClick = { vm.perform { logout() } }) { Text("Se déconnecter") }
                        state.error?.let { ErrorNotice(it, vm::clearMessage) }
                    }
                else
                    WorkspaceApp(
                        vm,
                        state,
                        snackbar,
                        sharedUrl,
                        consumedShare,
                        targetChat,
                        targetCacheScope,
                        consumedTarget,
                    )
            }
        }
    }
}

@Composable
private fun InstallationLabel(installation: Installation, modifier: Modifier = Modifier) {
    Column(modifier) {
        Text("${installation.name} · ${if (installation.online) "En ligne" else "Hors ligne"}")
        Text(installation.role.label, style = MaterialTheme.typography.labelSmall)
    }
}

@Composable
private fun WorkspaceApp(
    vm: LeoViewModel,
    state: Workspace,
    snackbar: SnackbarHostState,
    sharedUrl: String,
    consumedShare: () -> Unit,
    targetChat: String,
    targetCacheScope: String,
    consumedTarget: () -> Unit,
) {
    val nav = rememberNavController()
    fun openConnections() {
        if (state.isOwner) nav.navigate("connections")
        else vm.notify("Demandez au propriétaire de reconnecter le compte.")
    }

    val backStack by nav.currentBackStackEntryAsState()
    val route = backStack?.destination?.route ?: "fil"
    LaunchedEffect(sharedUrl) {
        if (sharedUrl.isNotBlank()) {
            if (state.isOwner) nav.navigate("authorize") { launchSingleTop = true }
            else {
                vm.notify("Cette action est réservée au propriétaire de l’installation.")
                consumedShare()
            }
        }
    }
    LaunchedEffect(targetChat, vm.api.cacheScope) {
        if (targetChat.isNotBlank() && targetCacheScope == vm.api.cacheScope) {
            nav.navigate("chat/${segment(targetChat)}") {
                // A pager may now display a different chat from its route's starting id.
                if (nav.currentDestination?.route == "chat/{id}") {
                    popUpTo("chat/{id}") { inclusive = true }
                }
                launchSingleTop = true
            }
            consumedTarget()
        }
    }
    var focusedContent by remember(route) { mutableStateOf(false) }
    val focused = focusedContent || focusedRoute(route)
    val selected = dockSelection(route)
    fun navigate(target: String) {
        nav.navigate(target) {
            popUpTo("fil") { saveState = true }
            launchSingleTop = true
            restoreState = true
        }
    }
    fun create() = nav.navigate("new-chat") { launchSingleTop = true }
    CompositionLocalProvider(
        LocalFocusMode provides { focusedContent = it },
        LocalSnackbar provides snackbar,
        LocalAgentPortraits provides rememberAgentPortraits(vm, state),
    ) {
        BoxWithConstraints(
            Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background)
        ) {
            val wide = maxWidth >= 700.dp
            Row {
                if (wide && !focusedContent)
                    NavigationRail(
                        Modifier.fillMaxHeight(),
                        containerColor = MaterialTheme.colorScheme.background,
                        header = {
                            Spacer(Modifier.height(16.dp))
                            RoundAction(
                                "Nouvelle conversation",
                                LeoIcons.Plus,
                                container = MaterialTheme.colorScheme.primary,
                                content = MaterialTheme.colorScheme.onPrimary,
                                outlined = false,
                                size = 52.dp,
                                onClick = ::create,
                            )
                            Spacer(Modifier.height(8.dp))
                        },
                    ) {
                        DockItems.forEach { d ->
                            NavigationRailItem(
                                selected == d.route,
                                { navigate(d.route) },
                                icon = { Icon(d.icon, d.label) },
                                label = { Text(d.label) },
                                colors =
                                    NavigationRailItemDefaults.colors(
                                        indicatorColor = MaterialTheme.colorScheme.primary,
                                        selectedIconColor = MaterialTheme.colorScheme.onPrimary,
                                        selectedTextColor = MaterialTheme.colorScheme.primary,
                                    ),
                            )
                        }
                    }
                Scaffold(
                    modifier = Modifier.weight(1f).imePadding(),
                    containerColor = MaterialTheme.colorScheme.background,
                    snackbarHost = { SnackbarHost(snackbar) },
                    topBar = {
                        if (!focused && route !in topLevel)
                            TopAppBar(
                                title = {},
                                navigationIcon = {
                                    IconButton(onClick = { nav.popBackStack() }) {
                                        Icon(Icons.AutoMirrored.Filled.ArrowBack, "Retour")
                                    }
                                },
                                actions = {
                                    IconButton(
                                        onClick = { vm.perform { refresh() } },
                                        enabled = !state.busy,
                                    ) {
                                        Icon(Icons.Default.Refresh, "Actualiser l’espace")
                                    }
                                },
                                colors =
                                    TopAppBarDefaults.topAppBarColors(
                                        containerColor = MaterialTheme.colorScheme.background
                                    ),
                            )
                    },
                    bottomBar = {
                        if (!wide && !focused && !WindowInsets.isImeVisible)
                            LeoDock(
                                selected,
                                ::navigate,
                                ::create,
                                Modifier.navigationBarsPadding(),
                            )
                    },
                ) { padding ->
                    Column(Modifier.padding(padding)) {
                        if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                        state.error?.let { ErrorNotice(it, vm::clearMessage) }
                        NavHost(
                            nav,
                            "fil",
                            Modifier.weight(1f),
                            // The default predictive back only scales the leaving screen, so its
                            // content stayed opaque while the screen below faded in.
                            predictivePopExitTransition = {
                                scaleOut(targetScale = 0.7f) + fadeOut(tween(700))
                            },
                        ) {
                            composable("fil") {
                                FilScreen(
                                    vm,
                                    state,
                                    openChat = { nav.navigate("chat/$it") },
                                    openRun = { nav.navigate("run/$it") },
                                    openConnections = ::openConnections,
                                    search = { nav.navigate("search") },
                                )
                            }
                            composable("search") {
                                SearchScreen(
                                    vm,
                                    state,
                                    back = { nav.popBackStack() },
                                    openChat = { nav.navigate("chat/$it") { popUpTo("fil") } },
                                    openRun = { nav.navigate("run/$it") { popUpTo("fil") } },
                                    openMission = { navigate("missions") },
                                    newChat = { agent ->
                                        nav.navigate("new-chat?agent=$agent&project=") {
                                            popUpTo("fil")
                                        }
                                    },
                                    open = { nav.navigate(it) { popUpTo("fil") } },
                                )
                            }
                            composable("chat/{id}") { entry ->
                                ChatScreen(
                                    vm,
                                    state,
                                    entry.arguments?.getString("id"),
                                    openChat = { nav.navigate("chat/$it") { popUpTo("fil") } },
                                    openRun = { nav.navigate("run/$it") },
                                    back = { nav.popBackStack() },
                                    create = { nav.navigate("new-chat") },
                                    openConnections = ::openConnections,
                                )
                            }
                            composable("new-chat?agent={agent}&project={project}") { entry ->
                                ChatScreen(
                                    vm,
                                    state,
                                    null,
                                    initialAgent =
                                        entry.arguments?.getString("agent")?.ifBlank { null }
                                            ?: MAIN_AGENT_ID,
                                    initialProject =
                                        entry.arguments?.getString("project").orEmpty(),
                                    openChat = { nav.navigate("chat/$it") { popUpTo("fil") } },
                                    openRun = { nav.navigate("run/$it") },
                                    back = { nav.popBackStack() },
                                    create = { nav.navigate("new-chat") },
                                    openConnections = ::openConnections,
                                )
                            }
                            composable("missions") {
                                MissionsScreen(vm, state) { nav.navigate("run/$it") }
                            }
                            composable("runs") { RunsScreen(vm, state) { nav.navigate("run/$it") } }
                            composable("run/{id}") { entry ->
                                RunScreen(
                                    vm,
                                    state,
                                    entry.arguments?.getString("id").orEmpty(),
                                    openChat = { nav.navigate("chat/$it") },
                                    back = { nav.popBackStack() },
                                ) {
                                    nav.navigate("run/$it") {
                                        popUpTo("run/{id}") { inclusive = true }
                                    }
                                }
                            }
                            composable("atelier") {
                                AtelierScreen(vm, state) { nav.navigate(it) }
                            }
                            composable("agents") {
                                ResourcesScreen(vm, state, true) { agent, project ->
                                    nav.navigate("new-chat?agent=$agent&project=$project")
                                }
                            }
                            composable("projects") {
                                ResourcesScreen(vm, state, false) { agent, project ->
                                    nav.navigate("new-chat?agent=$agent&project=$project")
                                }
                            }
                            if (state.isOwner) composable("mcps") { McpsScreen(vm, state) }
                            composable("skills") { SkillsScreen(vm, state) }
                            if (state.isOwner)
                                composable("connections") {
                                    ConnectionsScreen(vm, state) { nav.navigate("run/$it") }
                                }
                            if (state.isOwner) composable("nodes") { NodesScreen(vm, state) }
                            composable("settings") { SettingsScreen(vm, state) }
                            if (state.isOwner)
                                composable("authorize") {
                                    AuthorizeScreen(vm, state, sharedUrl, consumedShare)
                                }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun LoginScreen(vm: LeoViewModel, state: Workspace) {
    var email by rememberSaveable(state.origin) { mutableStateOf("") }
    // Verification codes never enter saved instance state.
    var code by remember(state.emailForCode) { mutableStateOf("") }
    Scaffold { padding ->
        Box(
            Modifier.padding(padding).imePadding().fillMaxSize(),
            contentAlignment = Alignment.Center,
        ) {
            Column(Modifier.widthIn(max = 520.dp)) {
                Page {
                    Wordmark()
                    Heading("Bienvenue dans Leo", "Connectez-vous à votre compte Leo.")
                    Panel {
                        if (state.origin.isBlank()) {
                            Text(
                                "Le service officiel n’est pas configuré dans cette version de l’application."
                            )
                        } else if (state.emailForCode == null) {
                            OutlinedTextField(
                                email,
                                { email = it },
                                Modifier.fillMaxWidth(),
                                label = { Text("Adresse e-mail") },
                                singleLine = true,
                                keyboardOptions =
                                    androidx.compose.foundation.text.KeyboardOptions(
                                        keyboardType =
                                            androidx.compose.ui.text.input.KeyboardType.Email
                                    ),
                            )
                            Button(
                                onClick = { vm.perform { requestEmailCode(email) } },
                                enabled = email.isNotBlank() && !state.busy,
                                modifier = Modifier.fillMaxWidth(),
                            ) {
                                Text("Recevoir un code")
                            }
                        } else {
                            Text("Un code a été envoyé à ${state.emailForCode}.")
                            OutlinedTextField(
                                code,
                                { code = it },
                                Modifier.fillMaxWidth(),
                                label = { Text("Code reçu par e-mail") },
                                singleLine = true,
                                keyboardOptions =
                                    androidx.compose.foundation.text.KeyboardOptions(
                                        keyboardType =
                                            androidx.compose.ui.text.input.KeyboardType.Number
                                    ),
                            )
                            Button(
                                onClick = {
                                    vm.perform {
                                        verifyEmailCode(code)
                                        code = ""
                                    }
                                },
                                enabled = code.isNotBlank() && !state.busy,
                                modifier = Modifier.fillMaxWidth(),
                            ) {
                                Text("Se connecter")
                            }
                            TextButton(
                                onClick = { vm.perform { requestEmailCode(state.emailForCode) } },
                                enabled = !state.busy,
                            ) {
                                Text("Renvoyer un code")
                            }
                            TextButton(onClick = vm::changeEmail, enabled = !state.busy) {
                                Text("Changer d’adresse e-mail")
                            }
                        }
                        if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                        state.error?.let { ErrorNotice(it, vm::clearMessage) }
                    }
                    Text(
                        "Les exécutions continuent sur vos installations lorsque l’application est fermée."
                    )
                }
            }
        }
    }
}

@Composable
fun RunCard(run: Run, open: (String) -> Unit) {
    SignalCard(
        Modifier.fillMaxWidth(),
        onClick = { open(run.id) },
        padding = PaddingValues(14.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            RunStatusTile(run.status)
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    run.title,
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 2,
                    overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                )
                Text(
                    "${date(run.createdAt)} · ${duration(run)}",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.width(8.dp))
            Status(run.status)
        }
    }
}

/** Square status mark shared by run history rows. */
@Composable
internal fun RunStatusTile(status: String, size: androidx.compose.ui.unit.Dp = 34.dp) {
    val tint =
        when (status) {
            "succeeded" -> signal.success
            "failed",
            "interrupted" -> signal.attention
            "running",
            "queued" -> MaterialTheme.colorScheme.primary
            else -> MaterialTheme.colorScheme.onSurfaceVariant
        }
    Box(
        Modifier.size(size)
            .clip(androidx.compose.foundation.shape.RoundedCornerShape(10.dp))
            .background(tint.copy(alpha = 0.14f)),
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            when (status) {
                "succeeded" -> LeoIcons.Check
                "failed",
                "interrupted" -> LeoIcons.Close
                "running",
                "queued" -> LeoIcons.Play
                else -> LeoIcons.Pause
            },
            null,
            Modifier.size(size * 0.45f),
            tint = tint,
        )
    }
}
